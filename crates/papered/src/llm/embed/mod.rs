pub mod batcher;
pub mod client;

pub use batcher::BatchingEmbedder;
pub use client::{EmbeddingClient, EmbeddingResult, embed_image_or_text, image_to_base64};
