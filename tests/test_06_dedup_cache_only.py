"""Finding 6 — capture-path embedder load must not download.

Embedder::from_hf_cache calls hf-hub ApiRepo::get, which downloads on a
missing cache with no ureq timeout. A hung model endpoint parks every
caller waiting on the OnceLock, so incident creation never fail-opens.
"""

from __future__ import annotations

from source import fn_body, read

PRE_FIX_EMBEDDER = """
    fn embedder() -> Option<Arc<Embedder>> {
        EMBEDDER
            .get_or_init(|| match Embedder::from_hf_cache() {
                Ok(e) => Some(Arc::new(e)),
"""


def resolve_embedder(*, cache_only: bool, assets_present: bool, download_hangs: bool):
    """Return the loaded embedder, None (fail-open), or 'blocked'."""
    if assets_present:
        return "ready"
    if cache_only:
        return None
    if download_hangs:
        return "blocked"
    return "ready"


def capture_path_is_cache_only(dedup_rs: str, embeddings_rs: str) -> bool:
    embedder = fn_body(dedup_rs, "embedder")
    if "from_hf_cache()" in embedder and "from_local_hf_cache" not in embedder:
        return False
    if "from_local_hf_cache" not in embedder:
        return False
    local = fn_body(embeddings_rs, "from_local_hf_cache")
    # Must resolve via the on-disk Cache, never hf-hub's downloading Api.
    downloads = "api::sync::Api" in local or "Api::new" in local
    cache_lookup = "Cache::from_env" in local or "hf_hub::Cache" in local
    return cache_lookup and not downloads


def test_pre_fix_download_hang_blocks_fail_open():
    """Negative control: missing assets + hung GET never returns None."""
    assert "from_hf_cache()" in PRE_FIX_EMBEDDER
    assert (
        resolve_embedder(cache_only=False, assets_present=False, download_hangs=True)
        == "blocked"
    )


def test_head_capture_dedup_uses_cache_only_loader():
    """Fails if dedup.rs still calls Embedder::from_hf_cache()."""
    dedup = read("crates/ptask-server/src/dedup.rs")
    embeddings = read("crates/ptask-distill/src/embeddings.rs")
    assert capture_path_is_cache_only(
        dedup, embeddings
    ), "capture embedder() must use cache-only resolution and return None when assets are missing"
    assert (
        resolve_embedder(cache_only=True, assets_present=False, download_hangs=True)
        is None
    )
