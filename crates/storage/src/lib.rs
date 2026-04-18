// SPDX-License-Identifier: Apache-2.0

//! Capytain storage layer.
//!
//! Hosts the `DbConn` trait, repository modules, the migration runner, and the
//! on-disk blob store for raw `.eml` bodies. All callers depend on the trait
//! surface here rather than on any concrete database driver.
