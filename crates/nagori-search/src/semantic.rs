use nagori_core::{EntryId, Result, SearchQuery};

#[derive(Debug, Clone, PartialEq)]
pub struct Embedding(pub Vec<f32>);

#[derive(Debug, Clone, PartialEq)]
pub struct SemanticSearchHit {
    pub entry_id: EntryId,
    pub score: f32,
}

pub trait SemanticIndexer: Send + Sync {
    fn upsert_embedding(&self, entry_id: EntryId, embedding: Embedding) -> Result<()>;
    fn delete_embedding(&self, entry_id: EntryId) -> Result<()>;
    fn search(&self, query: &SearchQuery) -> Result<Vec<SemanticSearchHit>>;
}
