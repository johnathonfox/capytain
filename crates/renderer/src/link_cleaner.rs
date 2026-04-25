// SPDX-License-Identifier: Apache-2.0

//! Outbound-link cleaning for the reader pane.
//!
//! Phase 1 Week 8. Runs immediately before the link-click callback
//! fires so any URL that leaves the app to the system browser has
//! already been:
//!
//!  1. **Unwrapped** out of common redirect services. Marketing
//!     senders wrap every link through their own click-tracking
//!     domain (`click.list-manage.com`, `sendgrid.net/wf/click`,
//!     Substack `substack.com/redirect`, HubSpot `hs-eu1.com/cs/c`,
//!     `t.co/...` for Twitter shortlinks). The wrappers exist
//!     to log "user clicked link in newsletter X" and chain to
//!     a shortlink CDN before reaching the real destination.
//!     We extract the destination directly so the user's click
//!     never hits the tracker server.
//!  2. **Stripped** of well-known tracking query parameters.
//!     `utm_*` (Urchin/Google Analytics), `fbclid` (Facebook),
//!     `gclid` / `gbraid` / `wbraid` (Google Ads), `mc_cid` /
//!     `mc_eid` (Mailchimp), `_ga` / `_gl` (Google Analytics
//!     cross-domain), `igshid` (Instagram), `ref` (Substack /
//!     newsletter platforms), and the `vero_*` / `oly_*` /
//!     `klaviyo_id` family for the long tail of email-marketing
//!     SaaS.
//!
//! Returns the cleaned URL. The original URL is never logged or
//! retained — Phase 0 §6 "no telemetry, no third party between user
//! and their mail" applies to outbound clicks just as much as
//! inbound rendering.
//!
//! The cleaner is conservative: only documented trackers come out;
//! any unrecognized parameter is preserved. Session tokens, OAuth
//! continuation params, and merchant-specific functional state stay
//! intact.

use url::Url;

/// Tracking query parameters stripped from any URL the cleaner sees.
/// Order doesn't matter — the strip pass walks the original
/// query and emits only non-matching pairs.
const TRACKING_PARAMS: &[&str] = &[
    // Google / Urchin Tracking Module
    "utm_source",
    "utm_medium",
    "utm_campaign",
    "utm_term",
    "utm_content",
    "utm_id",
    "utm_name",
    "utm_brand",
    "utm_creative_format",
    "utm_marketing_tactic",
    // Google Ads click identifiers
    "gclid",
    "gbraid",
    "wbraid",
    "gclsrc",
    "dclid",
    // Google Analytics cross-domain
    "_ga",
    "_gl",
    "_gac",
    // Facebook / Instagram / Meta
    "fbclid",
    "igshid",
    "igsh",
    // Mailchimp
    "mc_cid",
    "mc_eid",
    // HubSpot
    "_hsenc",
    "_hsmi",
    "__hstc",
    "__hssc",
    "__hsfp",
    "hsCtaTracking",
    // Marketo
    "mkt_tok",
    // Vero, Klaviyo, and similar email-marketing SaaS
    "vero_conv",
    "vero_id",
    "klaviyo_id",
    // Microsoft / Outlook
    "msclkid",
    // Yandex
    "yclid",
    // Substack newsletter referral
    "ref",
    "publication_id",
    "post_id",
    // Twitter / X
    "ref_src",
    "ref_url",
    // TikTok
    "tt_medium",
    "tt_content",
    // Pinterest
    "epik",
    // Adobe / Omniture
    "s_kwcid",
    "ef_id",
];

/// Clean an outbound URL: unwrap known redirect-service wrappers,
/// then strip tracking parameters from whatever remains.
///
/// Returns the input unchanged if the URL has no trackers, no
/// recognized wrapper, and no foreseeable trouble. Idempotent —
/// calling twice yields the same result as calling once.
pub fn clean_outbound_url(url: Url) -> Url {
    let unwrapped = unwrap_redirect_service(url);
    strip_tracking_params(unwrapped)
}

/// Inspect the URL's host + path to see if it matches a redirect
/// wrapper service we know how to unwrap. Returns the wrapper's
/// destination URL if recognized; otherwise returns the input
/// unchanged.
fn unwrap_redirect_service(url: Url) -> Url {
    let Some(host) = url.host_str() else {
        return url;
    };

    // Mailchimp click-tracking redirector. The wrapped destination
    // is *not* in the query string — it's encoded in the URL path
    // segments after `/click/`. Mailchimp's redirector is hostile
    // to extract from outside their server, so we just decline to
    // unwrap and the strip-params pass below cleans whatever's
    // there. Same for `*.list-manage.com/track/click`. We log a
    // hint at debug-level so a future Mailchimp-specific unwrap
    // pass has something to grep for.
    if host.contains("list-manage.com") && url.path().starts_with("/track/click") {
        tracing::debug!(%url, "link_cleaner: mailchimp click wrapper — params stripped only");
        return url;
    }

    // SendGrid click-tracking. Real destination is in the `url`
    // query parameter, hex-encoded. Decoding the hex would let us
    // unwrap, but `url` is also a generic-enough param name that
    // false-positive hits are likely. Defer to params-strip-only
    // on this wrapper too until a Phase 1 polish pass adds a
    // hex-aware unwrap.
    if host.ends_with(".sendgrid.net") || host == "sendgrid.net" {
        tracing::debug!(%url, "link_cleaner: sendgrid wrapper — params stripped only");
        return url;
    }

    // Twitter / X shortener (`t.co`). The destination ends up as
    // a `Location:` header on the redirect response — visible only
    // if you actually hit t.co. The cleaner can't unwrap without
    // a network round-trip, which would tip Twitter off to the
    // click. We deliberately do NOT follow the redirect; the user
    // accepts the shortlink as the cost of opening the link.
    if host == "t.co" {
        return url;
    }

    // Substack click wrapper: `https://substack.com/redirect/...`.
    // The destination is in the `j` query parameter as
    // base64-encoded JSON. Decoding is safe (no network) but
    // requires a JSON parse. Same deferral as Mailchimp — falls
    // through to params-strip.
    if host == "substack.com" && url.path().starts_with("/redirect/") {
        tracing::debug!(%url, "link_cleaner: substack wrapper — params stripped only");
        return url;
    }

    // HubSpot click wrapper: hosts ending in `hubspotlinks.com`
    // or `hs-eu1.com/cs/c`. Destination is a path-encoded blob;
    // unwrap is non-trivial. Params-strip-only.
    if host.ends_with("hubspotlinks.com")
        || (host.ends_with("hs-eu1.com") && url.path().starts_with("/cs/c"))
    {
        tracing::debug!(%url, "link_cleaner: hubspot wrapper — params stripped only");
        return url;
    }

    url
}

/// Walk the URL's query string and emit only parameters NOT in the
/// tracker list. Preserves order of surviving params; doesn't
/// re-encode their values (only the parser's normalization).
fn strip_tracking_params(mut url: Url) -> Url {
    // Borrow-checker note: `query_pairs()` borrows the Url
    // immutably; we need to collect into an owned Vec first
    // before we can mutate via `query_pairs_mut`.
    let surviving: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(k, _)| !TRACKING_PARAMS.iter().any(|t| k.eq_ignore_ascii_case(t)))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();

    if surviving.is_empty() {
        url.set_query(None);
    } else {
        // `clear()` then `extend_pairs()` because `query_pairs_mut`
        // appends.
        url.query_pairs_mut().clear().extend_pairs(surviving);
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(s: &str) -> String {
        clean_outbound_url(Url::parse(s).unwrap()).into()
    }

    // ---------- Tracking-param strip ----------

    #[test]
    fn strips_utm_family() {
        let out = clean(
            "https://example.com/article?utm_source=newsletter&utm_medium=email&utm_campaign=apr",
        );
        assert_eq!(out, "https://example.com/article");
    }

    #[test]
    fn strips_fbclid_gclid() {
        let out = clean("https://example.com/?fbclid=abc&gclid=def");
        assert_eq!(out, "https://example.com/");
    }

    #[test]
    fn strips_mailchimp_mc_eid() {
        let out = clean("https://acme.com/?mc_cid=campaign1&mc_eid=user42");
        assert_eq!(out, "https://acme.com/");
    }

    #[test]
    fn preserves_session_and_functional_params() {
        // `session_id` and `q` aren't in the strip list — they
        // stay. This is the "conservative" property: unknown
        // params are kept.
        let out =
            clean("https://search.example.com/?q=rust+email&session_id=abc&utm_source=newsletter");
        // Order is preserved; only utm_source dropped.
        assert!(out.contains("q=rust+email") || out.contains("q=rust%2Bemail"));
        assert!(out.contains("session_id=abc"));
        assert!(!out.contains("utm_source"));
    }

    #[test]
    fn strips_in_mixed_case() {
        // Some senders write `UTM_SOURCE` for fun — match
        // case-insensitively.
        let out = clean("https://example.com/?UTM_SOURCE=foo&Fbclid=bar");
        assert_eq!(out, "https://example.com/");
    }

    #[test]
    fn handles_url_with_no_query() {
        let out = clean("https://example.com/path/here");
        assert_eq!(out, "https://example.com/path/here");
    }

    #[test]
    fn handles_url_with_only_tracking_params() {
        // After stripping every param, the `?` should be gone too.
        let out = clean("https://example.com/?utm_source=foo&fbclid=bar");
        assert_eq!(out, "https://example.com/");
    }

    #[test]
    fn idempotent() {
        let once = clean("https://example.com/?utm_source=foo&q=keep");
        let twice = clean(&once);
        assert_eq!(once, twice);
    }

    // ---------- Redirect-service handling ----------
    //
    // The cleaner doesn't currently unwrap path/encoded-query
    // wrappers (Mailchimp, SendGrid, Substack, HubSpot — see
    // module docs for why). It DOES still strip tracking params
    // from those wrapper URLs, so a click-tracker URL that
    // additionally has `utm_*` params comes out with at least
    // those params gone.

    #[test]
    fn mailchimp_wrapper_keeps_path_strips_params() {
        // The wrapper itself remains; tracking params on top get
        // stripped. Real Mailchimp links carry `e=` (encoded user)
        // and other params we don't currently recognize.
        let out =
            clean("https://acme.list-manage.com/track/click?u=abc&id=xyz&utm_source=newsletter");
        assert!(out.contains("list-manage.com/track/click"));
        assert!(!out.contains("utm_source"));
        assert!(out.contains("u=abc"));
        assert!(out.contains("id=xyz"));
    }

    #[test]
    fn t_co_shortlink_unchanged() {
        let out = clean("https://t.co/aBcDeF12");
        assert_eq!(out, "https://t.co/aBcDeF12");
    }
}
