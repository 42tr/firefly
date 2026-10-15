use std::{fmt, io::IsTerminal};

use chrono::Utc;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::{
    fmt::{
        FmtContext,
        format::{self, FormatEvent, FormatFields},
        time::FormatTime,
    },
    registry::LookupSpan,
};

/// Format log timestamps in the configured application timezone.
struct AppTimer;

impl FormatTime for AppTimer {
    fn format_time(&self, w: &mut format::Writer<'_>) -> fmt::Result {
        let now = Utc::now().with_timezone(&crate::config::timezone());
        write!(w, "{}", now.format("%m-%d %H:%M:%S"))
    }
}

/// 自定义日志事件格式，确保时间和级别之间只有一个空格。
struct AppFormat;

impl<S, N> FormatEvent<S, N> for AppFormat
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    N: for<'a> FormatFields<'a> + 'static,
{
    fn format_event(
        &self, ctx: &FmtContext<'_, S, N>, mut writer: format::Writer<'_>, event: &Event<'_>,
    ) -> fmt::Result {
        if writer.has_ansi_escapes() {
            write!(writer, "\x1b[90m")?;
        }
        AppTimer.format_time(&mut writer)?;
        if writer.has_ansi_escapes() {
            write!(writer, "\x1b[0m")?;
        }

        let level = event.metadata().level();
        if writer.has_ansi_escapes() {
            let code = level_color(level);
            write!(writer, " [{}m{}[0m ", code, level)?;
        } else {
            write!(writer, " {} ", level)?;
        }

        let mut spans_seen = false;
        ctx.visit_spans(|span| -> fmt::Result {
            if spans_seen {
                write!(writer, ":")?;
            }
            spans_seen = true;
            write!(writer, "{}", span.metadata().name())?;
            Ok(())
        })?;

        if spans_seen {
            write!(writer, " ")?;
        }

        ctx.format_fields(writer.by_ref(), event)?;
        writeln!(writer)
    }
}

fn level_color(level: &Level) -> u8 {
    match *level {
        Level::TRACE => 35,
        Level::DEBUG => 34,
        Level::INFO => 32,
        Level::WARN => 33,
        Level::ERROR => 31,
    }
}

/// 初始化日志系统。
///
/// 使用 `tracing-subscriber` 作为后端。`tracing-subscriber` 默认会通过 `tracing-log`
/// 桥接旧的 `log` crate 日志，因此无需手动安装 `LogTracer`。
/// 日志级别由 `RUST_LOG` 环境变量控制,未设置时默认为 `info`；任务注册模块仅保留警告和错误。
pub fn init() {
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
        // 调度库每注册一个任务都会打印 "Job creator created"，屏蔽这类内部状态日志。
        .add_directive("tokio_cron_scheduler::job_scheduler=warn".parse().expect("valid scheduler log directive"));

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        // Journal and file output should not contain terminal color escapes.
        .with_ansi(std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none())
        .with_target(false)
        .event_format(AppFormat)
        .init();
}
