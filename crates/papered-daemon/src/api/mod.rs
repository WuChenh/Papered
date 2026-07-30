//! REST API routes for the Papered daemon.

pub mod annotations;
pub mod config;
pub mod export;
pub mod health;
pub mod health_response;
pub mod insights;
pub mod lattice;
pub mod metrics;
pub mod papers;
pub mod prompts;
pub mod rag;
pub mod search;
pub mod translations;
pub mod types;
pub mod zotero;

use axum::{
    Router,
    routing::{delete, get, post, put},
};
use papered::routes::*;
use std::sync::Arc;

use crate::AppState;

/// Build the REST API router.
pub fn api_router() -> Router<Arc<AppState>> {
    use self::annotations::{
        add_comment, delete_comment, delete_rating, get_rating, list_comments, set_rating,
    };
    use self::config::{
        get_config, import_queue, reset_data, setup_status, test_embedding, test_endpoint,
        test_reranker, update_config, update_embedding_config,
    };
    use self::export::export_data;
    use self::health::{
        cleanup_health, cleanup_images, data_quality, find_duplicate_groups, health, image_quality,
        kb_health, optimize_images, optimize_store, regenerate_covers, v1_health,
    };
    use self::insights::generate_insight;
    use self::lattice::{
        import_from_lattice, lattice_collections, lattice_search, lattice_status, lattice_sync,
        lattice_sync_cancel, lattice_sync_collections,
    };
    use self::metrics::metrics;
    use self::papers::{
        add_paper, batch_add_papers, batch_delete_papers, batch_paper_status,
        batch_reindex_papers_sections, delete_paper, get_figure_image, get_paper, get_paper_chunk,
        get_paper_cover, get_paper_cover_thumb, get_paper_figures, get_paper_sections,
        import_paper, list_papers, pick_file, regenerate_paper_cover, reindex_paper,
        reindex_paper_sections, stats, update_paper,
    };
    use self::prompts::{
        create_prompt, delete_prompt, get_prompt, list_prompts, set_default_prompt, update_prompt,
    };
    use self::rag::ask_rag;
    use self::search::{find_similar, paper_graph, search, search_figures, search_passages};
    use self::translations::{
        batch_translate, delete_translations, get_translations, search_translations,
        translate_paper,
    };
    use self::zotero::{
        zotero_collections, zotero_status, zotero_sync, zotero_sync_cancel,
        zotero_sync_collections, zotero_sync_status,
    };

    Router::new()
        .route(HEALTH, get(health))
        .route(API_CONFIG, get(get_config).put(update_config))
        .route(API_SETUP_STATUS, get(setup_status))
        .route(API_CONFIG_EMBEDDING, post(update_embedding_config))
        .route(API_PAPERS, get(list_papers).post(add_paper))
        .route(API_PAPERS_IMPORT, post(import_paper))
        .route(API_PAPERS_PICK_FILE, post(pick_file))
        .route(API_PAPERS_BATCH_DELETE, post(batch_delete_papers))
        .route(API_PAPERS_BATCH, post(batch_add_papers))
        .route(API_PAPERS_BATCH_STATUS, post(batch_paper_status))
        .route(
            API_PAPERS_BATCH_REINDEX_SECTIONS,
            post(batch_reindex_papers_sections),
        )
        .route(
            API_PAPERS_ID,
            get(get_paper).put(update_paper).delete(delete_paper),
        )
        .route(API_PAPERS_ID_SECTIONS, get(get_paper_sections))
        .route(API_PAPERS_ID_FIGURES, get(get_paper_figures))
        .route(API_PAPERS_ID_FIGURES_IMAGE, get(get_figure_image))
        .route(API_PAPERS_ID_COVER, get(get_paper_cover))
        .route(API_PAPERS_ID_COVER_THUMB, get(get_paper_cover_thumb))
        .route(API_PAPERS_ID_COVER_REGENERATE, post(regenerate_paper_cover))
        .route(API_PAPERS_ID_CHUNKS_CHUNK_ID, get(get_paper_chunk))
        .route(
            API_PAPERS_ID_RATING,
            get(get_rating).put(set_rating).delete(delete_rating),
        )
        .route(API_PAPERS_ID_COMMENTS, get(list_comments).post(add_comment))
        .route(API_PAPERS_ID_COMMENTS_ID, delete(delete_comment))
        .route(API_PAPERS_ID_INSIGHTS, post(generate_insight))
        .route(API_PAPERS_ID_REINDEX, post(reindex_paper))
        .route(API_PAPERS_ID_REINDEX_SECTIONS, post(reindex_paper_sections))
        .route(API_TEST_ENDPOINT, post(test_endpoint))
        .route(API_TEST_EMBEDDING, post(test_embedding))
        .route(API_TEST_RERANKER, post(test_reranker))
        .route(API_SEARCH, post(search))
        .route(API_SEARCH_FIGURES, post(search_figures))
        .route(API_SEARCH_PASSAGES, post(search_passages))
        .route(API_GRAPH, get(paper_graph))
        .route(API_SIMILAR, post(find_similar))
        .route(API_ASK, post(ask_rag))
        .route(API_PROMPTS, get(list_prompts).post(create_prompt))
        .route(
            API_PROMPTS_ID,
            get(get_prompt).put(update_prompt).delete(delete_prompt),
        )
        .route(API_PROMPTS_ID_DEFAULT, post(set_default_prompt))
        .route(API_STATS, get(stats))
        .route(API_METRICS, get(metrics))
        .route(API_IMPORT_QUEUE, get(import_queue))
        .route(API_LATTICE_STATUS, get(lattice_status))
        .route(API_LATTICE_COLLECTIONS, get(lattice_collections))
        .route(API_LATTICE_SEARCH, get(lattice_search))
        .route(API_LATTICE_SYNC, post(lattice_sync))
        .route(API_LATTICE_SYNC_CANCEL, post(lattice_sync_cancel))
        .route(API_LATTICE_SYNC_COLLECTIONS, put(lattice_sync_collections))
        .route(API_IMPORT_LATTICE, post(import_from_lattice))
        .route(API_HEALTH, get(v1_health))
        .route(API_HEALTH_KB, get(kb_health))
        .route(API_HEALTH_DUPLICATES, get(find_duplicate_groups))
        .route(API_HEALTH_QUALITY, get(data_quality))
        .route(API_HEALTH_IMAGE_QUALITY, get(image_quality))
        .route(API_HEALTH_CLEANUP_IMAGES, post(cleanup_images))
        .route(API_HEALTH_CLEANUP, post(cleanup_health))
        .route(API_HEALTH_REGENERATE_COVERS, post(regenerate_covers))
        .route(API_HEALTH_OPTIMIZE_IMAGES, post(optimize_images))
        .route(API_HEALTH_OPTIMIZE, post(optimize_store))
        .route(API_EXPORT, post(export_data))
        .route(
            API_PAPERS_ID_TRANSLATIONS,
            get(get_translations).delete(delete_translations),
        )
        .route(API_PAPERS_ID_TRANSLATE, post(translate_paper))
        .route(API_PAPERS_ID_TRANSLATE_BATCH, post(batch_translate))
        .route(API_TRANSLATIONS_SEARCH, get(search_translations))
        .route(API_ZOTERO_STATUS, get(zotero_status))
        .route(API_ZOTERO_COLLECTIONS, get(zotero_collections))
        .route(API_ZOTERO_SYNC_COLLECTIONS, put(zotero_sync_collections))
        .route(API_ZOTERO_SYNC, post(zotero_sync))
        .route(API_ZOTERO_SYNC_ID, get(zotero_sync_status))
        .route(API_ZOTERO_SYNC_CANCEL, post(zotero_sync_cancel))
        .route(API_RESET_DATA, post(reset_data))
}
