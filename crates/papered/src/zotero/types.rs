#![allow(non_snake_case)]

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoteroLibrary {
    #[serde(rename = "type")]
    pub library_type: String,
    pub id: u32,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoteroItem {
    pub key: String,
    pub version: u64,
    pub library: Option<ZoteroLibrary>,
    pub meta: Option<serde_json::Value>,
    pub data: ZoteroItemData,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ZoteroItemData {
    pub key: String,
    #[serde(rename = "itemType")]
    pub item_type: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub creators: Vec<ZoteroCreator>,
    #[serde(default)]
    pub abstract_note: Option<String>,
    pub date: Option<String>,
    pub DOI: Option<String>,
    pub url: Option<String>,
    #[serde(default)]
    pub tags: Vec<ZoteroTag>,
    #[serde(default)]
    pub collections: Vec<String>,
    pub dateAdded: Option<String>,
    pub dateModified: Option<String>,
    #[serde(flatten, default)]
    pub extra: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoteroCreator {
    #[serde(rename = "firstName")]
    pub first_name: Option<String>,
    #[serde(rename = "lastName")]
    pub last_name: Option<String>,
    #[serde(rename = "creatorType")]
    pub creator_type: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoteroTag {
    pub tag: String,
    #[serde(rename = "type")]
    pub tag_type: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoteroCollection {
    pub key: String,
    pub version: u64,
    pub data: ZoteroCollectionData,
}

fn deserialize_parent_collection<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(false) => Ok(None),
        serde_json::Value::String(s) => Ok(Some(s)),
        other => Err(serde::de::Error::custom(format!(
            "expected string, null, or false for parentCollection, got {other}"
        ))),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoteroCollectionData {
    pub key: String,
    pub name: String,
    #[serde(deserialize_with = "deserialize_parent_collection")]
    pub parentCollection: Option<String>,
    pub relations: Option<serde_json::Value>,
    pub dateAdded: Option<String>,
    pub dateModified: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoteroChildItem {
    pub key: String,
    pub version: u64,
    pub data: ZoteroChildData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZoteroChildData {
    pub key: String,
    #[serde(rename = "itemType")]
    pub item_type: String,
    pub title: Option<String>,
    pub parentItem: String,
    pub contentType: Option<String>,
    pub filename: Option<String>,
    pub charset: Option<String>,
    pub path: Option<String>,
    pub md5: Option<String>,
    pub mtime: Option<i64>,
    pub note: Option<String>,
    pub dateAdded: Option<String>,
    pub dateModified: Option<String>,
    pub tags: Option<Vec<ZoteroTag>>,
    pub relations: Option<serde_json::Value>,
    pub extra: Option<String>,
    pub links: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_zotero_item_data_with_all_fields() {
        let json = r#"{
            "key": "ITEM1234",
            "itemType": "journalArticle",
            "title": "Test Paper",
            "creators": [{"creatorType": "author", "firstName": "John", "lastName": "Doe"}],
            "collections": ["COLL1"],
            "date": "2024",
            "DOI": "10.1234/test"
        }"#;
        let data: ZoteroItemData = serde_json::from_str(json).unwrap();
        assert_eq!(data.key, "ITEM1234");
        assert_eq!(data.item_type, "journalArticle");
        assert_eq!(data.title, "Test Paper");
        assert_eq!(data.creators.len(), 1);
        assert_eq!(data.collections, vec!["COLL1"]);
    }

    #[test]
    fn test_zotero_item_data_missing_title() {
        let json = r#"{
            "key": "NOTE1234",
            "itemType": "note",
            "creators": [],
            "collections": []
        }"#;
        let data: ZoteroItemData = serde_json::from_str(json).unwrap();
        assert_eq!(data.title, "");
    }

    #[test]
    fn test_zotero_item_data_missing_creators() {
        let json = r#"{
            "key": "NOTE1234",
            "itemType": "note",
            "title": "",
            "collections": []
        }"#;
        let data: ZoteroItemData = serde_json::from_str(json).unwrap();
        assert!(data.creators.is_empty());
    }

    #[test]
    fn test_zotero_item_data_missing_collections() {
        let json = r#"{
            "key": "ATTACH1234",
            "itemType": "attachment",
            "title": "snapshot.html",
            "creators": []
        }"#;
        let data: ZoteroItemData = serde_json::from_str(json).unwrap();
        assert!(data.collections.is_empty());
    }

    #[test]
    fn test_zotero_item_data_missing_all_defaults() {
        // Simulates a minimal item returned by Zotero local API
        let json = r#"{
            "key": "MIN1234",
            "itemType": "note"
        }"#;
        let data: ZoteroItemData = serde_json::from_str(json).unwrap();
        assert_eq!(data.key, "MIN1234");
        assert_eq!(data.item_type, "note");
        assert_eq!(data.title, "");
        assert!(data.creators.is_empty());
        assert!(data.collections.is_empty());
        assert!(data.tags.is_empty());
    }
}
