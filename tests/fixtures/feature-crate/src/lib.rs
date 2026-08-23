pub fn always(value: bool) -> bool {
    value
}

#[cfg(feature = "extra")]
pub fn feature_only() -> bool {
    true
}
