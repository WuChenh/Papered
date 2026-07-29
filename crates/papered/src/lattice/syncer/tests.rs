use super::*;

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

fn id_set(list: &[&str]) -> HashSet<String> {
    list.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn plan_sync_empty_library_and_store() {
    let plan = plan_sync(&[], &HashSet::new());
    assert!(plan.to_fetch.is_empty());
    assert!(plan.to_remove.is_empty());
    assert_eq!(plan.up_to_date, 0);
}

#[test]
fn plan_sync_all_new() {
    let plan = plan_sync(&ids(&["a", "b", "c"]), &HashSet::new());
    assert_eq!(plan.to_fetch, ids(&["a", "b", "c"]));
    assert!(plan.to_remove.is_empty());
    assert_eq!(plan.up_to_date, 0);
}

#[test]
fn plan_sync_all_existing() {
    let plan = plan_sync(&ids(&["a", "b"]), &id_set(&["a", "b"]));
    assert!(plan.to_fetch.is_empty());
    assert!(plan.to_remove.is_empty());
    assert_eq!(plan.up_to_date, 2);
}

#[test]
fn plan_sync_mixed_new_existing_and_removed() {
    // "b" and "d" were imported before; "d" vanished from Lattice, "n1"/"n2"
    // are new.
    let plan = plan_sync(&ids(&["n1", "b", "n2"]), &id_set(&["b", "d"]));
    assert_eq!(plan.to_fetch, ids(&["n1", "n2"]));
    assert_eq!(plan.to_remove, ids(&["d"]));
    assert_eq!(plan.up_to_date, 1);
}

#[test]
fn plan_sync_dedups_enumerated_ids() {
    // Pagination overlap can repeat ids: a new id must be fetched once, and
    // an already-imported id counts once as up-to-date.
    let plan = plan_sync(&ids(&["a", "n", "a", "n"]), &id_set(&["a"]));
    assert_eq!(plan.to_fetch, ids(&["n"]));
    assert!(plan.to_remove.is_empty());
    assert_eq!(plan.up_to_date, 1);
}

#[test]
fn plan_sync_remove_list_is_sorted() {
    let plan = plan_sync(&ids(&["keep"]), &id_set(&["z", "keep", "m", "a"]));
    assert!(plan.to_fetch.is_empty());
    assert_eq!(plan.to_remove, ids(&["a", "m", "z"]));
    assert_eq!(plan.up_to_date, 1);
}

#[test]
fn plan_sync_emptied_library_removes_everything() {
    // The user deleted every paper from Lattice: enumeration is complete but
    // empty, so all imported papers must be removed.
    let plan = plan_sync(&[], &id_set(&["a", "b"]));
    assert!(plan.to_fetch.is_empty());
    assert_eq!(plan.to_remove, ids(&["a", "b"]));
    assert_eq!(plan.up_to_date, 0);
}

#[tokio::test]
async fn lattice_source_fetch_items_drains_buffers() {
    let detail = LatticePaperDetail {
        id: "p1".to_string(),
        citekey: "k1".to_string(),
        title: "T".to_string(),
        authors: Vec::new(),
        year: None,
        journal: None,
        doi: None,
        volume: None,
        issue: None,
        pages: None,
        isbn: None,
        paper_type: "article".to_string(),
        csl_item: None,
        pdf_path: None,
        abstract_text: None,
    };
    let source = LatticeSource::new(
        vec![detail],
        vec!["boom".to_string()],
        CancellationToken::new(),
    );
    let (items, errors) = source.fetch_items().await.unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, "p1");
    assert_eq!(errors, vec!["boom".to_string()]);
    // Buffers are drained: a second call returns nothing.
    let (items, errors) = source.fetch_items().await.unwrap();
    assert!(items.is_empty());
    assert!(errors.is_empty());
}

#[tokio::test]
async fn find_pdf_prefers_lattice_resolved_path() {
    // A temp file standing in for the Lattice-attached PDF.
    let pdf = std::env::temp_dir().join(format!(
        "papered_lattice_findpdf_{}.pdf",
        uuid::Uuid::new_v4()
    ));
    std::fs::write(&pdf, b"%PDF-1.4 fake").unwrap();

    let detail = LatticePaperDetail {
        id: "p1".to_string(),
        citekey: "k1".to_string(),
        title: "T".to_string(),
        authors: Vec::new(),
        year: None,
        journal: None,
        doi: None,
        volume: None,
        issue: None,
        pages: None,
        isbn: None,
        paper_type: "article".to_string(),
        csl_item: None,
        pdf_path: Some(pdf.to_string_lossy().into_owned()),
        abstract_text: None,
    };
    let source = LatticeSource::new(Vec::new(), Vec::new(), CancellationToken::new());

    // pdf_path exists and is accessible → returned directly.
    let found = source.find_pdf(&detail, &[]).await;
    assert_eq!(found, Some(pdf.clone()));

    // No pdf_path → falls back to filesystem search, which finds nothing in
    // the empty search paths.
    let mut detail_no_path = detail.clone();
    detail_no_path.pdf_path = None;
    let found = source.find_pdf(&detail_no_path, &[]).await;
    assert_eq!(found, None);

    std::fs::remove_file(&pdf).ok();
}

fn collection(id: &str, name: &str) -> LatticeCollection {
    LatticeCollection {
        id: id.to_string(),
        name: name.to_string(),
        path: name.to_string(),
        depth: 0,
    }
}

#[test]
fn resolve_collection_names_maps_names_to_ids() {
    let collections = vec![
        collection("id-a", "AI4S"),
        collection("id-b", "Reading List"),
    ];
    let resolved = resolve_collection_names_to_ids(
        &["AI4S".to_string(), "Reading List".to_string()],
        &collections,
    );
    assert_eq!(resolved, ids(&["id-a", "id-b"]));
}

#[test]
fn resolve_collection_names_ignores_unknown_names() {
    let collections = vec![collection("id-a", "AI4S")];
    let resolved =
        resolve_collection_names_to_ids(&["AI4S".to_string(), "Missing".to_string()], &collections);
    assert_eq!(resolved, ids(&["id-a"]));
}

#[test]
fn resolve_collection_names_returns_empty_when_nothing_matches() {
    let collections = vec![collection("id-a", "AI4S")];
    let resolved = resolve_collection_names_to_ids(&["Missing".to_string()], &collections);
    assert!(resolved.is_empty());
}

#[test]
fn plan_sync_dedups_multi_collection_overlap() {
    // Two collections both contain "shared"; the merged enumeration should
    // fetch it once, keep it once as up-to-date, and not remove it.
    let enumerated = ids(&["shared", "only-a", "shared", "only-b"]);
    let plan = plan_sync(&enumerated, &id_set(&["shared"]));
    assert_eq!(plan.to_fetch, ids(&["only-a", "only-b"]));
    assert!(plan.to_remove.is_empty());
    assert_eq!(plan.up_to_date, 1);
}
