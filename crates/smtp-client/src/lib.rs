// SPDX-License-Identifier: Apache-2.0

//! Capytain SMTP submission adapter.
//!
//! Wraps `lettre` for submission on port 587 (STARTTLS) or 465 (implicit TLS)
//! with SASL XOAUTH2. STARTTLS downgrade is never permitted.
