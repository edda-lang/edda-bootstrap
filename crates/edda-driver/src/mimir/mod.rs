//! Mímir package-management verb orchestration.
//!
//! Each sub-module implements one of the §8 verbs (plus slice F's `key`):
//! - [`add`] — §8.1 `edda add`
//! - [`update`] — §8.2 `edda update`
//! - [`audit`] — §8.3 `edda audit`
//! - [`publish`] — §8.4 `edda publish`
//! - [`contract_diff`] — §8.5 `edda contract-diff`
//! - [`why`] — §8.6 `edda why`
//! - [`key`] — slice F `edda key generate` (publisher-keystore management)
//!
//! These verbs share no cascade state with the build pipeline; they operate
//! directly on `package.toml` / `package.lock.toml` / registry / keystore.
//! The [`crate::run_mimir`] entry point handles all of them without
//! constructing a [`crate::Driver`].

pub mod add;
pub mod audit;
pub mod contract_diff;
pub mod key;
pub mod publish;
pub mod update;
pub mod why;
