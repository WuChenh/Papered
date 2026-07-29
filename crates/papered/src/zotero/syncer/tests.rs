use super::*;
use crate::paper::PaperStatus;
use crate::store::vector::*;
use crate::test_support::MockVectorStore;
use crate::zotero::{
    ZoteroChildItem, ZoteroCollection, ZoteroCollectionData, ZoteroCreator, ZoteroItem,
    ZoteroItemData, ZoteroItemListResponse, ZoteroTag,
};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ------------------------------------------------------------------
// MockZoteroApi — configurable fake Zotero server
// ------------------------------------------------------------------
#[derive(Debug, Clone, Default)]
struct MockZoteroApi {
    top_items: Arc<Mutex<Vec<ZoteroItem>>>,
    collections: Arc<Mutex<Vec<ZoteroCollection>>>,
    collection_items: Arc<Mutex<HashMap<String, Vec<ZoteroItem>>>>,
    children: Arc<Mutex<HashMap<String, Vec<ZoteroChildItem>>>>,
    files: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    fail_next: Arc<Mutex<Option<String>>>,
    fail_collection: Arc<Mutex<Option<String>>>,
    per_call_delay: Arc<Mutex<Option<std::time::Duration>>>,
}

impl MockZoteroApi {
    fn new() -> Self {
        Self::default()
    }
    fn set_top_items(&self, items: Vec<ZoteroItem>) {
        *self.top_items.lock().unwrap() = items;
    }
    fn set_collections(&self, collections: Vec<ZoteroCollection>) {
        *self.collections.lock().unwrap() = collections;
    }
    fn set_collection_items(&self, key: String, items: Vec<ZoteroItem>) {
        self.collection_items.lock().unwrap().insert(key, items);
    }
    fn set_children(&self, parent_key: String, children: Vec<ZoteroChildItem>) {
        self.children.lock().unwrap().insert(parent_key, children);
    }
    fn set_fail_next(&self, reason: String) {
        *self.fail_next.lock().unwrap() = Some(reason);
    }
    fn set_fail_collection(&self, key: String) {
        *self.fail_collection.lock().unwrap() = Some(key);
    }
    fn set_per_call_delay(&self, delay: std::time::Duration) {
        *self.per_call_delay.lock().unwrap() = Some(delay);
    }
    async fn maybe_delay(&self) {
        let delay = *self.per_call_delay.lock().unwrap();
        if let Some(delay) = delay {
            tokio::time::sleep(delay).await;
        }
    }
    fn check_fail(&self) -> Result<()> {
        if let Some(ref reason) = *self.fail_next.lock().unwrap() {
            return Err(crate::error::PaperedError::Unknown(format!(
                "mock failure: {reason}"
            )));
        }
        Ok(())
    }
    fn check_collection_fail(&self, collection_key: &str) -> Result<()> {
        if let Some(ref fail_key) = *self.fail_collection.lock().unwrap()
            && fail_key == collection_key
        {
            return Err(crate::error::PaperedError::Unknown(format!(
                "mock collection failure: {collection_key}"
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl ZoteroApi for MockZoteroApi {
    async fn list_top_items(&self, _limit: u32, _since: u64) -> Result<ZoteroItemListResponse> {
        self.maybe_delay().await;
        self.check_fail()?;
        let items = self.top_items.lock().unwrap().clone();
        Ok(ZoteroItemListResponse {
            last_modified_version: items.iter().map(|i| i.version).max().unwrap_or(0),
            items,
        })
    }
    async fn list_collections(&self) -> Result<Vec<ZoteroCollection>> {
        self.check_fail()?;
        Ok(self.collections.lock().unwrap().clone())
    }
    async fn get_collection_items(
        &self,
        collection_key: &str,
        _limit: u32,
        _since: u64,
    ) -> Result<ZoteroItemListResponse> {
        self.maybe_delay().await;
        self.check_fail()?;
        self.check_collection_fail(collection_key)?;
        let items = self
            .collection_items
            .lock()
            .unwrap()
            .get(collection_key)
            .cloned()
            .unwrap_or_default();
        Ok(ZoteroItemListResponse {
            last_modified_version: items.iter().map(|i| i.version).max().unwrap_or(0),
            items,
        })
    }
    async fn get_collection_top_items(
        &self,
        collection_key: &str,
        _limit: u32,
        _since: u64,
    ) -> Result<ZoteroItemListResponse> {
        self.maybe_delay().await;
        self.check_fail()?;
        self.check_collection_fail(collection_key)?;
        let items = self
            .collection_items
            .lock()
            .unwrap()
            .get(collection_key)
            .cloned()
            .unwrap_or_default();
        Ok(ZoteroItemListResponse {
            last_modified_version: items.iter().map(|i| i.version).max().unwrap_or(0),
            items,
        })
    }
    async fn get_children(&self, parent_key: &str) -> Result<Vec<ZoteroChildItem>> {
        self.maybe_delay().await;
        self.check_fail()?;
        Ok(self
            .children
            .lock()
            .unwrap()
            .get(parent_key)
            .cloned()
            .unwrap_or_default())
    }
    async fn download_file(&self, item_key: &str) -> Result<Vec<u8>> {
        self.maybe_delay().await;
        self.check_fail()?;
        self.files
            .lock()
            .unwrap()
            .get(item_key)
            .cloned()
            .ok_or_else(|| {
                crate::error::PaperedError::NotFound(
                    format!("mock file not found: {item_key}"),
                    None,
                )
            })
    }
}

// ------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------
fn make_item(key: &str, title: &str, version: u64) -> ZoteroItem {
    ZoteroItem {
        key: key.to_string(),
        version,
        library: None,
        meta: None,
        data: ZoteroItemData {
            key: key.to_string(),
            item_type: "journalArticle".to_string(),
            title: title.to_string(),
            creators: vec![ZoteroCreator {
                first_name: Some("Alice".to_string()),
                last_name: Some("Smith".to_string()),
                creator_type: "author".to_string(),
                name: None,
            }],
            abstract_note: Some("An abstract.".to_string()),
            date: Some("2024".to_string()),
            DOI: Some("10.1234/test".to_string()),
            url: Some("https://example.com".to_string()),
            tags: vec![ZoteroTag {
                tag: "tag1".to_string(),
                tag_type: None,
            }],
            collections: vec![],
            dateAdded: None,
            dateModified: None,
            extra: HashMap::new(),
        },
    }
}

fn make_child_pdf(parent_key: &str, child_key: &str, path: Option<&str>) -> ZoteroChildItem {
    ZoteroChildItem {
        key: child_key.to_string(),
        version: 1,
        data: crate::zotero::types::ZoteroChildData {
            key: child_key.to_string(),
            item_type: "attachment".to_string(),
            title: Some("paper.pdf".to_string()),
            parentItem: parent_key.to_string(),
            contentType: Some("application/pdf".to_string()),
            filename: Some("paper.pdf".to_string()),
            charset: None,
            path: path.map(std::string::ToString::to_string),
            md5: None,
            mtime: None,
            note: None,
            dateAdded: None,
            dateModified: None,
            tags: None,
            relations: None,
            extra: None,
            links: None,
        },
    }
}

fn setup_syncer(
    store: Arc<dyn VectorStore>,
    api: MockZoteroApi,
) -> (ZoteroSyncer, mpsc::Receiver<crate::util::IndexJob>) {
    let (tx, rx) = mpsc::channel(100);
    let syncer = ZoteroSyncer::with_client(
        Box::new(api),
        store,
        tx,
        50,
        vec![],
        vec![],
        false,
        0,
        false,
        CancellationToken::new(),
    );
    (syncer, rx)
}

// ------------------------------------------------------------------
// Tests
// ------------------------------------------------------------------

#[tokio::test]
async fn test_sync_imports_new_item_with_pdf() {
    let store: Arc<dyn VectorStore> = Arc::new(MockVectorStore::default());
    let api = MockZoteroApi::new();

    let tmp = tempfile::NamedTempFile::with_suffix(".pdf").unwrap();
    let pdf_path = tmp.path().to_string_lossy().into_owned();

    let item = make_item("ITEM1", "Test Paper", 1);
    api.set_top_items(vec![item.clone()]);
    api.set_children(
        "ITEM1".to_string(),
        vec![make_child_pdf("ITEM1", "CHILD1", Some(&pdf_path))],
    );

    let (mut syncer, mut rx) = setup_syncer(store.clone(), api);

    let report = syncer.sync().await;

    assert_eq!(report.imported, 1);
    assert_eq!(report.pdf_found, 1);
    assert_eq!(report.skipped, 0);
    assert!(report.errors.is_empty(), "errors: {:?}", report.errors);
    assert_eq!(report.new_since, 1);

    // Verify paper was stored
    let papers = store.list_papers(100, 0).await.unwrap();
    assert_eq!(papers.len(), 1);
    let paper = &papers[0];
    assert_eq!(paper.title, "Test Paper");
    assert_eq!(paper.source, Some(PaperSource::Zotero));
    assert!(paper.file_path.is_some());

    // Verify index job was queued
    let job = rx.try_recv().expect("index job should be queued");
    assert_eq!(job.paper_id, paper.id);
}

#[tokio::test]
async fn test_sync_skips_item_without_pdf() {
    let store: Arc<dyn VectorStore> = Arc::new(MockVectorStore::default());
    let api = MockZoteroApi::new();

    let item = make_item("ITEM1", "Test Paper", 1);
    api.set_top_items(vec![item]);

    let (mut syncer, _rx) = setup_syncer(store.clone(), api);

    let report = syncer.sync().await;

    assert_eq!(report.imported, 0);
    assert_eq!(report.skipped, 1);
    assert!(report.errors.is_empty());

    let papers = store.list_papers(100, 0).await.unwrap();
    assert_eq!(papers.len(), 0);
}

#[tokio::test]
async fn test_sync_skips_item_with_empty_title() {
    let store: Arc<dyn VectorStore> = Arc::new(MockVectorStore::default());
    let api = MockZoteroApi::new();

    let mut item = make_item("ITEM1", "", 1);
    item.data.title = String::new();
    api.set_top_items(vec![item]);

    let (mut syncer, _rx) = setup_syncer(store.clone(), api);

    let report = syncer.sync().await;

    assert_eq!(report.imported, 0);
    assert_eq!(report.skipped, 1);
    assert!(report.errors.is_empty());
}

#[tokio::test]
async fn test_sync_updates_existing_metadata() {
    let store: Arc<dyn VectorStore> = Arc::new(MockVectorStore::default());
    let api = MockZoteroApi::new();

    let tmp = tempfile::NamedTempFile::with_suffix(".pdf").unwrap();
    let pdf_path = tmp.path().to_string_lossy().into_owned();

    // First sync
    let item = make_item("ITEM1", "Original Title", 1);
    api.set_top_items(vec![item.clone()]);
    api.set_children(
        "ITEM1".to_string(),
        vec![make_child_pdf("ITEM1", "CHILD1", Some(&pdf_path))],
    );

    let (mut syncer, mut rx) = setup_syncer(store.clone(), api.clone());
    let report1 = syncer.sync().await;
    assert_eq!(report1.imported, 1);
    rx.try_recv().unwrap(); // consume job

    // Second sync with updated metadata
    let mut updated_item = item;
    updated_item.version = 2;
    updated_item.data.title = "Updated Title".to_string();
    updated_item.data.DOI = Some("10.9999/new".to_string());
    api.set_top_items(vec![updated_item.clone()]);

    // Re-create syncer with last_sync_version=1 to simulate incremental sync
    let (mut syncer2, _rx2) = setup_syncer(store.clone(), api);
    syncer2.last_sync_version = 1;

    let report2 = syncer2.sync().await;
    assert_eq!(report2.imported, 0); // already exists
    assert_eq!(report2.skipped, 0); // metadata was updated

    let papers = store.list_papers(100, 0).await.unwrap();
    assert_eq!(papers.len(), 1);
    // Title should NOT change because Papered data takes precedence
    assert_eq!(papers[0].title, "Original Title");
    // DOI was already set during first import, so it should NOT change
    assert_eq!(papers[0].doi, Some("10.1234/test".to_string()));
}

#[tokio::test]
async fn test_sync_removes_missing_papers() {
    let store: Arc<dyn VectorStore> = Arc::new(MockVectorStore::default());
    let api = MockZoteroApi::new();

    let tmp = tempfile::NamedTempFile::with_suffix(".pdf").unwrap();
    let pdf_path = tmp.path().to_string_lossy().into_owned();

    // Import two items
    let item1 = make_item("ITEM1", "Paper One", 1);
    let item2 = make_item("ITEM2", "Paper Two", 2);
    api.set_top_items(vec![item1.clone(), item2.clone()]);
    api.set_children(
        "ITEM1".to_string(),
        vec![make_child_pdf("ITEM1", "C1", Some(&pdf_path))],
    );
    api.set_children(
        "ITEM2".to_string(),
        vec![make_child_pdf("ITEM2", "C2", Some(&pdf_path))],
    );

    let (mut syncer, mut rx) = setup_syncer(store.clone(), api.clone());
    let report1 = syncer.sync().await;
    assert_eq!(report1.imported, 2);
    rx.try_recv().unwrap();
    rx.try_recv().unwrap();

    // Now remove ITEM2 from Zotero
    api.set_top_items(vec![item1]);

    let (mut syncer2, _rx2) = setup_syncer(store.clone(), api);
    let report2 = syncer2.sync().await;
    assert_eq!(report2.removed, 1);

    let papers = store.list_papers(100, 0).await.unwrap();
    assert_eq!(papers.len(), 1);
    assert!(papers[0].extra.as_ref().unwrap().contains("ITEM1"));
}

#[tokio::test]
async fn test_sync_does_not_advance_last_sync_version_on_error() {
    let store: Arc<dyn VectorStore> = Arc::new(MockVectorStore::default());
    let api = MockZoteroApi::new();
    api.set_fail_next("network error".to_string());

    let (mut syncer, _rx) = setup_syncer(store.clone(), api);

    let report = syncer.sync().await;

    assert!(!report.errors.is_empty());
    // last_sync_version should remain 0 because sync failed
    assert_eq!(report.new_since, 0);
}

#[tokio::test]
async fn test_sync_collection_filter() {
    let store: Arc<dyn VectorStore> = Arc::new(MockVectorStore::default());
    let api = MockZoteroApi::new();

    let tmp = tempfile::NamedTempFile::with_suffix(".pdf").unwrap();
    let pdf_path = tmp.path().to_string_lossy().into_owned();

    let item1 = make_item("ITEM1", "In Collection", 1);
    api.set_collection_items("COLL1".to_string(), vec![item1.clone()]);
    api.set_children(
        "ITEM1".to_string(),
        vec![make_child_pdf("ITEM1", "C1", Some(&pdf_path))],
    );

    let (tx, _rx) = mpsc::channel(100);
    let mut syncer = ZoteroSyncer::with_client(
        Box::new(api),
        store.clone(),
        tx,
        50,
        vec![],
        vec!["COLL1".to_string()],
        false,
        0,
        false,
        CancellationToken::new(),
    );

    let report = syncer.sync().await;

    assert_eq!(report.imported, 1);
    let papers = store.list_papers(100, 0).await.unwrap();
    assert_eq!(papers.len(), 1);
    assert_eq!(papers[0].title, "In Collection");
}

#[tokio::test]
async fn test_sync_recursive_collections() {
    let store: Arc<dyn VectorStore> = Arc::new(MockVectorStore::default());
    let api = MockZoteroApi::new();

    let tmp = tempfile::NamedTempFile::with_suffix(".pdf").unwrap();
    let pdf_path = tmp.path().to_string_lossy().into_owned();

    // Parent collection
    let parent = ZoteroCollection {
        key: "PARENT".to_string(),
        version: 1,
        data: ZoteroCollectionData {
            key: "PARENT".to_string(),
            name: "Parent".to_string(),
            parentCollection: None,
            relations: None,
            dateAdded: None,
            dateModified: None,
        },
    };
    // Child collection
    let child = ZoteroCollection {
        key: "CHILD".to_string(),
        version: 1,
        data: ZoteroCollectionData {
            key: "CHILD".to_string(),
            name: "Child".to_string(),
            parentCollection: Some("PARENT".to_string()),
            relations: None,
            dateAdded: None,
            dateModified: None,
        },
    };
    // Grandchild collection
    let grandchild = ZoteroCollection {
        key: "GRANDCHILD".to_string(),
        version: 1,
        data: ZoteroCollectionData {
            key: "GRANDCHILD".to_string(),
            name: "Grandchild".to_string(),
            parentCollection: Some("CHILD".to_string()),
            relations: None,
            dateAdded: None,
            dateModified: None,
        },
    };
    api.set_collections(vec![parent, child, grandchild]);

    let item_parent = make_item("ITEM_PARENT", "In Parent", 1);
    let item_child = make_item("ITEM_CHILD", "In Child", 2);
    let item_grandchild = make_item("ITEM_GRANDCHILD", "In Grandchild", 3);

    api.set_collection_items("PARENT".to_string(), vec![item_parent.clone()]);
    api.set_collection_items("CHILD".to_string(), vec![item_child.clone()]);
    api.set_collection_items("GRANDCHILD".to_string(), vec![item_grandchild.clone()]);

    api.set_children(
        "ITEM_PARENT".to_string(),
        vec![make_child_pdf("ITEM_PARENT", "C1", Some(&pdf_path))],
    );
    api.set_children(
        "ITEM_CHILD".to_string(),
        vec![make_child_pdf("ITEM_CHILD", "C2", Some(&pdf_path))],
    );
    api.set_children(
        "ITEM_GRANDCHILD".to_string(),
        vec![make_child_pdf("ITEM_GRANDCHILD", "C3", Some(&pdf_path))],
    );

    let (tx, _rx) = mpsc::channel(100);
    let mut syncer = ZoteroSyncer::with_client(
        Box::new(api),
        store.clone(),
        tx,
        50,
        vec![],
        vec!["PARENT".to_string()],
        false,
        0,
        true, // recursive_collections enabled
        CancellationToken::new(),
    );

    let report = syncer.sync().await;

    assert_eq!(report.imported, 3);
    let papers = store.list_papers(100, 0).await.unwrap();
    assert_eq!(papers.len(), 3);
    let titles: HashSet<_> = papers.iter().map(|p| p.title.clone()).collect();
    assert!(titles.contains("In Parent"));
    assert!(titles.contains("In Child"));
    assert!(titles.contains("In Grandchild"));
}

#[tokio::test]
async fn test_sync_cancellation() {
    let store: Arc<dyn VectorStore> = Arc::new(MockVectorStore::default());
    let cancel = CancellationToken::new();
    let api = MockZoteroApi::new();

    let tmp = tempfile::NamedTempFile::with_suffix(".pdf").unwrap();
    let pdf_path = tmp.path().to_string_lossy().into_owned();

    let item1 = make_item("ITEM1", "One", 1);
    let item2 = make_item("ITEM2", "Two", 2);
    api.set_top_items(vec![item1.clone(), item2.clone()]);
    api.set_children(
        "ITEM1".to_string(),
        vec![make_child_pdf("ITEM1", "C1", Some(&pdf_path))],
    );
    api.set_children(
        "ITEM2".to_string(),
        vec![make_child_pdf("ITEM2", "C2", Some(&pdf_path))],
    );

    // Cancel BEFORE creating syncer, so the sync stops immediately.
    cancel.cancel();

    let (tx, _rx) = mpsc::channel(100);
    let mut syncer = ZoteroSyncer::with_client(
        Box::new(api),
        store.clone(),
        tx,
        50,
        vec![],
        vec![],
        false,
        0,
        false,
        cancel,
    );

    let report = syncer.sync().await;

    // Cancelled immediately, so no items should be imported
    assert_eq!(report.imported, 0);
    assert!(
        report.errors.iter().any(|e| e.contains("cancelled")),
        "expected cancellation error, got: {:?}",
        report.errors
    );
}

#[tokio::test]
async fn test_sync_cancellation_mid_run() {
    let store: Arc<dyn VectorStore> = Arc::new(MockVectorStore::default());
    let cancel = CancellationToken::new();
    let api = MockZoteroApi::new();

    // Slow down each API call so the sync does not finish before cancellation.
    api.set_per_call_delay(std::time::Duration::from_millis(50));

    let tmp = tempfile::NamedTempFile::with_suffix(".pdf").unwrap();
    let pdf_path = tmp.path().to_string_lossy().into_owned();

    let mut items = Vec::new();
    for i in 0..20 {
        let key = format!("ITEM{i:02}");
        items.push(make_item(&key, &format!("Paper {i}"), i as u64 + 1));
        api.set_children(
            key.clone(),
            vec![make_child_pdf(&key, &format!("C{i}"), Some(&pdf_path))],
        );
    }
    api.set_top_items(items);

    let (tx, _rx) = mpsc::channel(100);
    let mut syncer = ZoteroSyncer::with_client(
        Box::new(api),
        store.clone(),
        tx,
        50,
        vec![],
        vec![],
        false,
        0,
        false,
        cancel.clone(),
    );

    let sync_handle = tokio::spawn(async move { syncer.sync().await });

    // Let the sync start processing items, then request cancellation.
    tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    cancel.cancel();

    let report = sync_handle.await.unwrap();

    // Some items may have been imported before cancellation, but not all 20.
    assert!(
        report.imported < 20,
        "expected partial import after mid-sync cancellation, got {} imported",
        report.imported
    );
    assert!(
        report.errors.iter().any(|e| e.contains("cancelled")),
        "expected cancellation error, got: {:?}",
        report.errors
    );
}

#[tokio::test]
async fn test_sync_preserves_file_path_when_worker_closed() {
    let store: Arc<dyn VectorStore> = Arc::new(MockVectorStore::default());
    let (tx, rx) = mpsc::channel::<crate::util::IndexJob>(1);
    drop(rx); // close the receiver so sends fail

    let api = MockZoteroApi::new();

    let tmp = tempfile::NamedTempFile::with_suffix(".pdf").unwrap();
    let pdf_path = tmp.path().to_string_lossy().into_owned();

    let item = make_item("ITEM1", "Test", 1);
    api.set_top_items(vec![item.clone()]);
    api.set_children(
        "ITEM1".to_string(),
        vec![make_child_pdf("ITEM1", "C1", Some(&pdf_path))],
    );

    let mut syncer = ZoteroSyncer::with_client(
        Box::new(api),
        store.clone(),
        tx,
        50,
        vec![],
        vec![],
        false,
        0,
        false,
        CancellationToken::new(),
    );

    let report = syncer.sync().await;

    assert_eq!(report.imported, 1);
    assert_eq!(report.pdf_found, 0); // channel closed → metadata_only
    assert_eq!(report.metadata_only, 1);

    let papers = store.list_papers(100, 0).await.unwrap();
    assert_eq!(papers.len(), 1);
    let paper = &papers[0];
    assert_eq!(paper.status, PaperStatus::Failed);
    assert!(paper.error_message.as_ref().unwrap().contains("worker"));
    assert!(
        paper.file_path.is_some(),
        "file_path should be preserved for retry"
    );
}

#[tokio::test]
async fn test_sync_library_wide_no_collection_filter() {
    let store: Arc<dyn VectorStore> = Arc::new(MockVectorStore::default());
    let api = MockZoteroApi::new();

    let tmp = tempfile::NamedTempFile::with_suffix(".pdf").unwrap();
    let pdf_path = tmp.path().to_string_lossy().into_owned();

    let items = vec![
        make_item("A", "Paper A", 1),
        make_item("B", "Paper B", 2),
        make_item("C", "Paper C", 3),
    ];
    api.set_top_items(items.clone());
    for item in &items {
        api.set_children(
            item.data.key.clone(),
            vec![make_child_pdf(&item.data.key, "child", Some(&pdf_path))],
        );
    }

    let (mut syncer, _rx) = setup_syncer(store.clone(), api);
    let report = syncer.sync().await;
    assert_eq!(report.imported, 3);
    assert_eq!(report.pdf_found, 3);
    assert!(report.errors.is_empty());

    let papers = store.list_papers(100, 0).await.unwrap();
    assert_eq!(papers.len(), 3);
}

#[tokio::test]
async fn test_sync_skip_cleanup_when_collection_fetch_fails() {
    let store: Arc<dyn VectorStore> = Arc::new(MockVectorStore::default());
    let api = MockZoteroApi::new();

    let tmp = tempfile::NamedTempFile::with_suffix(".pdf").unwrap();
    let pdf_path = tmp.path().to_string_lossy().into_owned();

    // Set up COLL1 with items
    let item1 = make_item("ITEM1", "In Coll1", 1);
    api.set_collection_items("COLL1".to_string(), vec![item1.clone()]);
    api.set_children(
        "ITEM1".to_string(),
        vec![make_child_pdf("ITEM1", "C1", Some(&pdf_path))],
    );

    // COLL2 will fail
    api.set_fail_collection("COLL2".to_string());

    let (tx, _rx) = mpsc::channel(100);
    let mut syncer = ZoteroSyncer::with_client(
        Box::new(api),
        store.clone(),
        tx,
        50,
        vec![],
        vec!["COLL1".to_string(), "COLL2".to_string()],
        false,
        0,
        false,
        CancellationToken::new(),
    );

    let report = syncer.sync().await;

    // COLL1 items should still be imported
    assert_eq!(report.imported, 1);
    // But there should be an error for COLL2
    assert!(report.errors.iter().any(|e| e.contains("COLL2")));
    // Cleanup should be skipped because not all collections succeeded
    assert_eq!(report.removed, 0);
}

#[tokio::test]
async fn test_sync_skips_attachment_and_note_items() {
    let store: Arc<dyn VectorStore> = Arc::new(MockVectorStore::default());
    let api = MockZoteroApi::new();

    let tmp = tempfile::NamedTempFile::with_suffix(".pdf").unwrap();
    let pdf_path = tmp.path().to_string_lossy().into_owned();

    let mut attachment = make_item("ATT1", "PDF", 1);
    attachment.data.item_type = "attachment".to_string();
    let mut note = make_item("NOTE1", "Some note", 2);
    note.data.item_type = "note".to_string();
    let article = make_item("ART1", "Real Paper", 3);

    api.set_top_items(vec![attachment, note, article.clone()]);
    api.set_children(
        "ART1".to_string(),
        vec![make_child_pdf("ART1", "C1", Some(&pdf_path))],
    );

    let (mut syncer, _rx) = setup_syncer(store.clone(), api);
    let report = syncer.sync().await;

    // Only the real article should be imported
    assert_eq!(report.imported, 1);
    assert_eq!(report.skipped, 2); // attachment + note
    assert_eq!(report.pdf_found, 1);
    assert!(report.errors.is_empty());

    let papers = store.list_papers(100, 0).await.unwrap();
    assert_eq!(papers.len(), 1);
    assert_eq!(papers[0].title, "Real Paper");
}

#[tokio::test]
async fn test_sync_skips_duplicate_pdf() {
    let store: Arc<dyn VectorStore> = Arc::new(MockVectorStore::default());

    // Pre-insert a paper with the same PDF path (simulating a manual import)
    let tmp = tempfile::NamedTempFile::with_suffix(".pdf").unwrap();
    let pdf_path = tmp.path().to_string_lossy().into_owned();
    let mut existing = Paper::new("Already Imported");
    existing.file_path = Some(pdf_path.clone());
    store.insert_paper(&existing).await.unwrap();

    let api = MockZoteroApi::new();
    let item = make_item("ZOT1", "Same PDF from Zotero", 1);
    api.set_top_items(vec![item.clone()]);
    api.set_children(
        "ZOT1".to_string(),
        vec![make_child_pdf("ZOT1", "C1", Some(&pdf_path))],
    );

    let (mut syncer, _rx) = setup_syncer(store.clone(), api);
    let report = syncer.sync().await;

    assert_eq!(report.imported, 0);
    assert_eq!(report.skipped, 1); // duplicate PDF
    assert!(report.errors.is_empty());

    let papers = store.list_papers(100, 0).await.unwrap();
    assert_eq!(papers.len(), 1); // still only one paper
    assert_eq!(papers[0].title, "Already Imported");
}
