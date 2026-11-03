//! The Aliyun /models catalog does not advertise effort controls. Read the
//! official parameter documentation, matching exact model IDs and protocols.
use std::{
    collections::BTreeMap,
    sync::OnceLock,
    time::{Duration, Instant},
};

use futures_util::StreamExt;
use scraper::{ElementRef, Html, Selector};
use serde_json::Value;

use crate::agent_settings::{ProfileModel, ReasoningOptions};

const RESPONSES: &str = "https://help.aliyun.com/zh/model-studio/qwen-api-via-openai-responses";
const ANTHROPIC: &str = "https://help.aliyun.com/zh/model-studio/anthropic-api-messages";
const LEVELS: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];
type Models = BTreeMap<String, ReasoningOptions>;

#[derive(Clone, Default)]
pub(super) struct Catalog {
    responses: Models,
    anthropic: Models,
}

impl Catalog {
    pub(super) fn apply(&self, models: &mut [ProfileModel]) {
        for model in models {
            // Explicit API capabilities (including []) take precedence.
            if model.reasoning_levels.is_some() {
                continue;
            }
            for (protocol, catalog) in [
                ("responses", &self.responses),
                ("anthropic", &self.anthropic),
            ] {
                if let Some(options) = catalog.get(&model.id) {
                    model
                        .reasoning_by_protocol
                        .insert(protocol.into(), options.clone());
                }
            }
        }
    }
}

pub(super) async fn load() -> Catalog {
    static CACHE: OnceLock<tokio::sync::Mutex<Option<(Instant, Catalog)>>> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().await;
    if let Some((_, catalog)) = cache
        .as_ref()
        .filter(|(refresh, _)| *refresh > Instant::now())
    {
        return catalog.clone();
    }
    // Public documentation requests have no provider credentials and no redirects.
    let Ok(client) = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(6))
        .connect_timeout(Duration::from_secs(3))
        .build()
    else {
        return cache
            .as_ref()
            .map(|(_, catalog)| catalog.clone())
            .unwrap_or_default();
    };
    let (responses, anthropic) = tokio::join!(
        fetch(&client, RESPONSES, false),
        fetch(&client, ANTHROPIC, true)
    );
    let complete = responses.is_some() && anthropic.is_some();
    // Retain the last successful metadata through temporary documentation outages.
    let mut catalog = cache
        .as_ref()
        .map(|(_, catalog)| catalog.clone())
        .unwrap_or_default();
    if let Some(models) = responses {
        catalog.responses = models;
    }
    if let Some(models) = anthropic {
        catalog.anthropic = models;
    }
    let ttl = if complete {
        Duration::from_secs(3600)
    } else {
        Duration::from_secs(60)
    };
    *cache = Some((Instant::now() + ttl, catalog.clone()));
    catalog
}

async fn fetch(client: &reqwest::Client, url: &str, anthropic: bool) -> Option<Models> {
    let response = client.get(url).send().await.ok()?.error_for_status().ok()?;
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.ok()?;
        if body.len() + chunk.len() > 4 * 1024 * 1024 {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    let models = parse(&String::from_utf8_lossy(&body), anthropic)?;
    (!models.is_empty()).then_some(models)
}

fn selector(query: &str) -> Selector {
    Selector::parse(query).unwrap()
}
fn text(node: ElementRef<'_>) -> String {
    node.text().collect::<String>()
}
fn children<'a>(node: ElementRef<'a>, tag: &str) -> Vec<ElementRef<'a>> {
    node.children()
        .filter_map(ElementRef::wrap)
        .filter(|child| child.value().name() == tag)
        .collect()
}
fn options(levels: Vec<String>, default: Option<String>) -> Option<ReasoningOptions> {
    let levels: Vec<_> = LEVELS
        .iter()
        .filter(|level| levels.iter().any(|item| item == **level))
        .map(|level| (*level).into())
        .collect();
    if levels.is_empty() {
        return None;
    }
    Some(ReasoningOptions {
        default_reasoning_level: default.filter(|level| levels.contains(level)),
        reasoning_levels: levels,
    })
}

fn parse(page: &str, anthropic: bool) -> Option<Models> {
    let props = page.split_once("window.__ICE_PAGE_PROPS__=")?.1;
    let value = serde_json::Deserializer::from_str(props)
        .into_iter::<Value>()
        .next()?
        .ok()?;
    let html = Html::parse_fragment(
        value
            .pointer("/docDetailData/storeData/data/content")?
            .as_str()?,
    );
    Some(if anthropic {
        parse_anthropic(&html)
    } else {
        parse_responses(&html)
    })
}

fn parse_responses(html: &Html) -> Models {
    let mut models = Models::new();
    for table in html.select(&selector("table")) {
        let Some(header) = table.select(&selector("tr")).next() else {
            continue;
        };
        let headings: Vec<_> = children(header, "th").into_iter().map(text).collect();
        if headings
            .iter()
            .map(|heading| heading.trim())
            .collect::<Vec<_>>()
            != ["模型", "支持的档位", "其余档位的映射"]
        {
            continue;
        }
        for row in table.select(&selector("tr")) {
            let cells = children(row, "td");
            if cells.len() != 3 {
                continue;
            }
            let levels: Vec<_> = cells[1].select(&selector("code")).map(text).collect();
            let description = text(cells[1]);
            let default = levels
                .iter()
                .find(|level| {
                    description
                        .split_once(level.as_str())
                        .is_some_and(|(_, suffix)| suffix.trim_start().starts_with("（默认）"))
                })
                .cloned();
            let Some(options) = options(levels, default) else {
                continue;
            };
            for id in cells[0].select(&selector("code")).map(text) {
                models.insert(id.trim().into(), options.clone());
            }
        }
    }
    models
}

fn parse_anthropic(html: &Html) -> Models {
    let mut models = Models::new();
    for section in html.select(&selector("section.collapse")) {
        if section
            .select(&selector("strong"))
            .next()
            .map(text)
            .as_deref()
            != Some("effort")
        {
            continue;
        }
        // Read only the effort parameter's immediate model list, excluding the
        // format parameter and the paragraphs describing aliases/mappings.
        for list in children(section, "div")
            .into_iter()
            .flat_map(|div| children(div, "ul"))
            .take(1)
        {
            for row in children(list, "li") {
                let Some(label) = children(row, "p").first().copied() else {
                    continue;
                };
                let description = text(label);
                if !description.contains("默认值") {
                    continue;
                }
                let default = label
                    .select(&selector("code"))
                    .map(text)
                    .find(|value| LEVELS.contains(&value.as_str()));
                let levels = children(row, "ul")
                    .into_iter()
                    .flat_map(|list| children(list, "li"))
                    .filter_map(|item| item.select(&selector("code")).next().map(text))
                    .collect();
                let Some(options) = options(levels, default) else {
                    continue;
                };
                let names = description.split(['（', '(']).next().unwrap_or_default();
                for id in names
                    .split(|c: char| !c.is_ascii_alphanumeric() && !matches!(c, '-' | '_' | '.'))
                    .filter(|id| id.contains('-'))
                {
                    models.insert(id.into(), options.clone());
                }
            }
        }
    }
    models
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn page(content: &str) -> String {
        format!(
            "<script>window.__ICE_PAGE_PROPS__={};otherScript()</script>",
            json!({"docDetailData":{"storeData":{"data":{"content":content}}}})
        )
    }

    #[test]
    fn reads_exact_model_efforts_and_default_without_mapping_aliases() {
        let models = parse(&page(r#"<table><tr><th>模型</th><th>支持的档位</th><th>其余档位的映射</th></tr>
            <tr><td><code>qwen3.8-max</code>、<code>qwen3.8-flash</code></td>
            <td><code>none</code>、<code>low</code>、<code>medium</code>、<code>xhigh</code>（默认）</td>
            <td><code>minimal</code> → <code>low</code>，<code>high</code>、<code>max</code> → <code>xhigh</code></td></tr></table>"#), false).unwrap();
        assert_eq!(
            models["qwen3.8-max"].reasoning_levels,
            ["none", "low", "medium", "xhigh"]
        );
        assert_eq!(
            models["qwen3.8-flash"].default_reasoning_level.as_deref(),
            Some("xhigh")
        );
        assert!(!models.contains_key("qwen3.7-max"));
        assert!(!models.contains_key("qwen3.8-max-preview"));
        assert!(parse("<html>login</html>", false).is_none());
    }

    #[test]
    fn anthropic_controls_are_separate_from_responses_and_api_metadata_wins() {
        let anthropic = parse(&page(r#"<section class="collapse"><p>属性</p><div><p><strong>effort</strong></p><ul>
            <li><p>glm-5.2、deepseek-v4-pro（默认值为 <code>max</code>）：</p><ul><li><code>high</code></li><li><code>max</code></li></ul>
            <p><code>low</code>、<code>medium</code> → <code>high</code>，<code>xhigh</code> → <code>max</code></p></li>
            <li><p>qwen3.8-max/qwen3.8-flash（默认值为 <code>xhigh</code>）：</p><ul><li><code>xhigh</code></li><li><code>medium</code></li><li><code>low</code></li></ul></li>
            </ul><p><strong>format</strong></p><ul><li><code>fake</code></li></ul></div></section>"#), true).unwrap();
        assert_eq!(anthropic["glm-5.2"].reasoning_levels, ["high", "max"]);
        assert_eq!(
            anthropic["deepseek-v4-pro"]
                .default_reasoning_level
                .as_deref(),
            Some("max")
        );
        assert_eq!(
            anthropic["qwen3.8-flash"].reasoning_levels,
            ["low", "medium", "xhigh"]
        );
        let catalog = Catalog {
            anthropic,
            responses: BTreeMap::from([(
                "glm-5.2".into(),
                ReasoningOptions {
                    reasoning_levels: vec!["none".into(), "high".into(), "max".into()],
                    default_reasoning_level: Some("high".into()),
                },
            )]),
        };
        let mut models = vec![
            ProfileModel::from("glm-5.2"),
            ProfileModel::from("qwen3.7-max"),
            ProfileModel {
                id: "qwen3.8-max".into(),
                reasoning_levels: Some(vec![]),
                ..Default::default()
            },
        ];
        catalog.apply(&mut models);
        assert_eq!(models[0].reasoning_options("responses").1, Some("high"));
        assert_eq!(models[0].reasoning_options("anthropic").1, Some("max"));
        assert!(models[1].reasoning_by_protocol.is_empty());
        assert!(models[2].reasoning_by_protocol.is_empty());
    }
}
