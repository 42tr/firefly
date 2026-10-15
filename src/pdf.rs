use std::collections::BTreeMap;

use anyhow::anyhow;
use lopdf::{Document, Object};
use tokio::task;

/// 将多个 PDF 字节缓冲区按顺序合并为一个 PDF。
pub async fn merge_pdf_buffers(buffers: Vec<Vec<u8>>) -> anyhow::Result<Vec<u8>> {
    if buffers.is_empty() {
        return Err(anyhow!("没有可合并的 PDF"));
    }
    task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        let mut max_id = 1;
        let mut documents_pages: BTreeMap<lopdf::ObjectId, Object> = BTreeMap::new();
        let mut documents_objects = BTreeMap::new();

        for bytes in buffers {
            let mut doc = Document::load_mem(&bytes)?;
            doc.renumber_objects_with(max_id);
            max_id = doc.max_id + 1;

            for (_, object_id) in doc.get_pages() {
                let page_object = doc.get_object(object_id)?.to_owned();
                documents_pages.insert(object_id, page_object);
            }
            documents_objects.extend(doc.objects);
        }

        let mut document = Document::with_version("1.5");
        let mut catalog_object: Option<(lopdf::ObjectId, Object)> = None;
        let mut pages_object: Option<(lopdf::ObjectId, Object)> = None;

        for (object_id, object) in documents_objects.into_iter() {
            match object.type_name().unwrap_or(b"") {
                b"Catalog" => {
                    catalog_object = Some((catalog_object.map(|(id, _)| id).unwrap_or(object_id), object));
                }
                b"Pages" => {
                    if let Ok(dictionary) = object.as_dict() {
                        let mut dictionary = dictionary.clone();
                        if let Some((_, ref existing)) = pages_object {
                            if let Ok(existing_dict) = existing.as_dict() {
                                dictionary.extend(existing_dict);
                            }
                        }
                        pages_object =
                            Some((pages_object.map(|(id, _)| id).unwrap_or(object_id), Object::Dictionary(dictionary)));
                    }
                }
                b"Page" | b"Outlines" | b"Outline" => {}
                _ => {
                    document.objects.insert(object_id, object);
                }
            }
        }

        let (catalog_id, catalog_obj) = catalog_object.ok_or_else(|| anyhow!("源 PDF 缺少 Catalog，无法合并"))?;
        let (pages_id, pages_obj) = pages_object.ok_or_else(|| anyhow!("源 PDF 缺少 Pages，无法合并"))?;

        for (object_id, object) in documents_pages.iter() {
            if let Ok(dictionary) = object.as_dict() {
                let mut dictionary = dictionary.clone();
                dictionary.set("Parent", pages_id);
                document.objects.insert(*object_id, Object::Dictionary(dictionary));
            }
        }

        if let Ok(dictionary) = pages_obj.as_dict() {
            let mut dictionary = dictionary.clone();
            dictionary.set("Count", documents_pages.len() as u32);
            dictionary.set("Kids", documents_pages.keys().map(|id| Object::Reference(*id)).collect::<Vec<_>>());
            document.objects.insert(pages_id, Object::Dictionary(dictionary));
        }

        if let Ok(dictionary) = catalog_obj.as_dict() {
            let mut dictionary = dictionary.clone();
            dictionary.set("Pages", pages_id);
            dictionary.remove(b"Outlines");
            document.objects.insert(catalog_id, Object::Dictionary(dictionary));
        }

        document.trailer.set("Root", catalog_id);
        document.max_id = document.objects.len() as u32;
        document.renumber_objects();
        document.adjust_zero_pages();
        document.compress();

        let mut output = Vec::new();
        document.save_to(&mut output)?;
        Ok(output)
    })
    .await
    .map_err(|err| anyhow!("合并 PDF 失败: {}", err))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    fn single_page(width: i64) -> Vec<u8> {
        let mut document = Document::with_version("1.5");
        let pages = document.new_object_id();
        let page = document.add_object(dictionary! {
            "Type" => "Page", "Parent" => pages,
            "MediaBox" => vec![0.into(), 0.into(), width.into(), 200.into()],
        });
        document.objects.insert(
            pages,
            dictionary! {
                "Type" => "Pages", "Kids" => vec![Object::Reference(page)], "Count" => 1,
            }
            .into(),
        );
        let catalog = document.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
        document.trailer.set("Root", catalog);
        let mut bytes = Vec::new();
        document.save_to(&mut bytes).unwrap();
        bytes
    }

    #[tokio::test]
    async fn merged_pdf_preserves_page_order_and_rejects_invalid_inputs() {
        let merged = merge_pdf_buffers(vec![single_page(100), single_page(300)])
            .await
            .unwrap();
        let document = Document::load_mem(&merged).unwrap();
        let widths: Vec<_> = document
            .get_pages()
            .values()
            .map(|id| {
                document
                    .get_object(*id)
                    .unwrap()
                    .as_dict()
                    .unwrap()
                    .get(b"MediaBox")
                    .unwrap()
                    .as_array()
                    .unwrap()[2]
                    .as_i64()
                    .unwrap()
            })
            .collect();
        assert_eq!(widths, vec![100, 300]);
        assert!(merge_pdf_buffers(vec![]).await.is_err());
        assert!(merge_pdf_buffers(vec![b"not a PDF".to_vec()])
            .await
            .is_err());
    }
}
