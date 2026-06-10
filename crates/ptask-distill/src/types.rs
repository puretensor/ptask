//! Shared pipeline data types that must stay available without the
//! `native-ml` feature: HAL consolidation (an HTTP stage, no candle)
//! consumes the cluster shape that the gated clustering module produces.

/// One topical cluster.
#[derive(Debug, Clone)]
pub struct Cluster {
    pub id: i64,
    pub keywords: Vec<String>,
    pub items: Vec<String>,
    pub item_sources: Vec<serde_json::Value>,
}
