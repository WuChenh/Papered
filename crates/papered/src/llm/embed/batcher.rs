use crate::error::Result;
use crate::llm::embed::{EmbeddingClient, EmbeddingResult};
use tokio::sync::oneshot;

type BatchRequest = (Vec<String>, oneshot::Sender<Result<Vec<EmbeddingResult>>>);

pub struct BatchingEmbedder {
    sender: tokio::sync::mpsc::Sender<BatchRequest>,
}

impl BatchingEmbedder {
    pub fn new(client: EmbeddingClient, batch_size: usize, flush_interval_ms: u64) -> Self {
        let capacity = batch_size * 4;
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<BatchRequest>(capacity);
        let flush_interval = std::time::Duration::from_millis(flush_interval_ms);

        tokio::spawn(async move {
            let mut buffer: Vec<BatchRequest> = Vec::new();
            let mut last_flush = std::time::Instant::now();

            loop {
                let timeout = tokio::time::sleep(flush_interval);
                tokio::pin!(timeout);

                tokio::select! {
                    req = receiver.recv() => {
                        match req {
                            Some((texts, tx)) => {
                                let total_texts: usize = buffer.iter().map(|(t, _)| t.len()).sum();
                                if total_texts + texts.len() > batch_size && !buffer.is_empty() {
                                    Self::flush_batch(&client, &mut buffer).await;
                                    last_flush = std::time::Instant::now();
                                }
                                buffer.push((texts, tx));
                            }
                            None => break,
                        }
                    }
                    _ = &mut timeout => {
                        if !buffer.is_empty() && last_flush.elapsed() >= flush_interval {
                            Self::flush_batch(&client, &mut buffer).await;
                            last_flush = std::time::Instant::now();
                        }
                    }
                }
            }
        });

        Self { sender }
    }

    async fn flush_batch(client: &EmbeddingClient, buffer: &mut Vec<BatchRequest>) {
        let mut all_texts: Vec<&str> = Vec::new();
        let mut boundaries: Vec<usize> = Vec::new();
        for (texts, _) in buffer.iter() {
            boundaries.push(texts.len());
            for t in texts {
                all_texts.push(t.as_str());
            }
        }

        let results = client.embed_batch(&all_texts).await;

        let mut offset = 0;
        for (i, (_, tx)) in buffer.drain(..).enumerate() {
            let count = boundaries[i];
            let batch_results = match &results {
                Ok(r) if r.len() >= offset + count => Ok(r[offset..offset + count].to_vec()),
                Ok(r) => Err(crate::error::PaperedError::EmbeddingApi {
                    status: 500,
                    message: format!(
                        "Embedding batch truncated: expected {} results, got {}",
                        count,
                        r.len()
                    ),
                }),
                Err(e) => Err(crate::error::PaperedError::EmbeddingApi {
                    status: 500,
                    message: e.to_string(),
                }),
            };
            let _ = tx.send(batch_results);
            offset += count;
        }
    }

    pub async fn embed_batch(
        &self,
        texts: &[&str],
    ) -> Result<Vec<crate::llm::embed::EmbeddingResult>> {
        let (tx, rx) = oneshot::channel();
        let texts: Vec<String> = texts.iter().map(|s| s.to_string()).collect();
        self.sender.send((texts, tx)).await.map_err(|_| {
            crate::error::PaperedError::EmbeddingApi {
                status: 500,
                message: "Batching embedder channel closed".to_string(),
            }
        })?;
        rx.await
            .map_err(|_| crate::error::PaperedError::EmbeddingApi {
                status: 500,
                message: "Batching embedder task dropped".to_string(),
            })?
    }
}
