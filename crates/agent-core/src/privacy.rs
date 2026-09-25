//! Privacy policy shared by native clients.

#[cfg_attr(feature = "bindings", uniffi::export)]
pub fn privacy_policy() -> String {
    include_str!("../../../docs/PRIVACY.md").into()
}
