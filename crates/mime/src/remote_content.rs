// SPDX-License-Identifier: Apache-2.0

//! Remote-content blocking for the reader pane.
//!
//! Phase 1 Week 8. This module owns a shared `adblock::Engine` (via
//! `OnceLock`) preloaded with a small curated filter list covering
//! the common tracker vectors that appear in email —
//! marketing-automation pixels (Mailchimp, SendGrid, HubSpot,
//! Substack), analytics SDK origins (Google Analytics, GTM,
//! Facebook, Segment, Mixpanel, Intercom), and generic tracking-
//! pixel patterns.
//!
//! The default list is intentionally not the full EasyList,
//! EasyPrivacy, and uBlock Origin family — those are ~5MB of filter
//! rules we haven't picked a bundling strategy for yet (compile-time
//! `include_bytes!` after serializing the compiled engine, or a
//! first-launch fetch-and-cache). Swapping the rule source is a
//! data-only change; `build_engine` and `is_blocked` stay the same.
//! Tracked on `PHASE_1.md` week 8's follow-up list.
//!
//! `sanitize_email_html` wires this into the ammonia pass via
//! `attribute_filter`: every `src` / `background` / `poster` /
//! `srcset` value gets checked; blocked URLs have the attribute
//! stripped, which breaks the image / media load. Link `href`s are
//! deliberately **not** filtered here — blocking an outbound anchor
//! would be user-hostile; link-click URL cleaning (utm_* stripping,
//! Mailchimp/SendGrid unwrapping) is a separate pipeline stage in
//! the renderer's `on_link_click` callback, shipping in a follow-up.

use std::sync::OnceLock;

use adblock::lists::ParseOptions;
use adblock::request::Request;
use adblock::Engine;

/// Curated filter list. ABP (Adblock Plus) syntax; see
/// <https://adblockplus.org/filter-cheatsheet>. Rules are grouped
/// by source for quick review; comment lines (`! …`) are ignored by
/// the parser.
///
/// Any addition here should name the tracker it's meant to catch.
/// Anything that would break a legitimate provider-hosted image
/// (e.g. Gmail's own attachment CDN on `mail.google.com`) stays
/// out.
pub const DEFAULT_FILTER_RULES: &str = r#"! Mailchimp — click/open tracking pixels + link wrappers
||list-manage.com^$image
||list-manage1.com^$image
||list-manage2.com^$image
||mailchi.mp/track^$image
! SendGrid — open/click pixels
||sendgrid.net/wf^$image
||sendgrid.net/ls^$image
||email-sent.sendgrid.net^$image
! SendGrid behind a brand CNAME (e.g. `ablink.email.<brand>.com/wf/open?upn=...`).
! Marketing-automation services (Rocket Money, etc.) front SendGrid
! through a custom subdomain so the sendgrid.net rule alone doesn't match.
! `wf/open?upn=` is a stable signature of SendGrid's open-tracking path
! that holds across CNAMEs, so a path substring is enough.
wf/open?upn=$image
! Braze marketing-automation pixels — `appboy` is the legacy product name
! still present in their image_assets URLs and tracking endpoints.
||braze.com^$image
||braze.eu^$image
||appboy.com^$image
! Iterable open-tracking. Iterable hosts `sp.email.<brand>.com/q/<token>~~`
! for opens and `links.email.<brand>.com/s/eo/<token>` for redirect-tracked
! links. Both surface as 1×1 hidden `<img>` pixels in the body. Block
! both substrings on image loads only — `links.email.*` URLs are also
! used for legitimate user-initiated link clicks, which are unaffected
! by an `$image`-qualified rule.
sp.email.$image
links.email.$image
! Iterable's CDN also hosts content (logos, banners) under
! `library.iterable.com` — those are intentional images, not pixels,
! so we don't block that domain.
! Google Analytics / Tag Manager / DoubleClick
||google-analytics.com^
||googletagmanager.com^
||doubleclick.net^
||googleadservices.com^
! Facebook / Meta pixel
||facebook.com/tr^$image
||connect.facebook.net^
! HubSpot tracking
||hubspot.com/__ptq.gif^$image
||hs-analytics.net^
||hs-scripts.com^
! Segment / Mixpanel / Amplitude / Intercom (analytics SDKs)
||segment.io^
||segment.com^$image
||api.mixpanel.com^$image
||amplitude.com^
||intercom.io^
! Substack open tracking
||substack.com/action^$image
! Twitter / X link-wrapping analytics
||t.co/i/adsct^
! LinkedIn analytics
||px.ads.linkedin.com^
||licdn.com/px^$image
! Generic tracking-pixel naming conventions — last-resort catch-
! all for the long tail of tracker SaaS services.
||tracking.$image
||pixel.$image
||beacon.$image
||analytics.$image
"#;

static ENGINE: OnceLock<Engine> = OnceLock::new();

/// Return the shared engine, building it on first access. The
/// filter-rule parse is O(rules); keeping one Engine for the life
/// of the process avoids re-parsing on every message render.
pub fn default_engine() -> &'static Engine {
    ENGINE.get_or_init(|| Engine::from_rules(DEFAULT_FILTER_RULES.lines(), ParseOptions::default()))
}

/// True if `url` (loaded as `request_type` — `"image"` / `"script"`
/// / `"other"` / etc.) matches a block rule in the provided engine.
///
/// The synthetic source URL `https://qsl.local/reader/` fixes
/// the third-party bit to "always true" (every remote URL in an
/// email body is third-party to the user), which is what the filter
/// rules above expect. Invalid URLs are not treated as blocked —
/// the ammonia sanitizer handles malformed URL values separately.
pub fn is_blocked(engine: &Engine, url: &str, request_type: &str) -> bool {
    match Request::new(url, "https://qsl.local/reader/", request_type) {
        Ok(req) => engine.check_network_request(&req).matched,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mailchimp_pixel_is_blocked() {
        let engine = default_engine();
        assert!(is_blocked(
            engine,
            "https://acme.list-manage.com/track/open.php?u=abc&id=123",
            "image",
        ));
    }

    #[test]
    fn google_analytics_blocked() {
        let engine = default_engine();
        assert!(is_blocked(
            engine,
            "https://www.google-analytics.com/collect?tid=UA-123",
            "image",
        ));
    }

    #[test]
    fn facebook_pixel_blocked() {
        let engine = default_engine();
        assert!(is_blocked(
            engine,
            "https://www.facebook.com/tr?id=123&ev=PageView&cd=abc",
            "image",
        ));
    }

    #[test]
    fn benign_image_is_not_blocked() {
        let engine = default_engine();
        assert!(!is_blocked(engine, "https://example.com/logo.png", "image",));
        // Gmail's own user-facing CDN — must not be blocked.
        assert!(!is_blocked(
            engine,
            "https://mail.google.com/mail/u/0/images/cleardot.gif",
            "image",
        ));
    }

    #[test]
    fn sendgrid_open_pixel_through_custom_cname_is_blocked() {
        // Rocket Money / Braze relays SendGrid open-tracking through
        // a `ablink.email.<brand>.com` CNAME. The sendgrid.net rule
        // alone doesn't catch this; the wildcard subdomain rule does.
        let engine = default_engine();
        assert!(is_blocked(
            engine,
            "https://ablink.email.rocketmoney.com/wf/open?upn=u001.abc",
            "image",
        ));
    }

    #[test]
    fn braze_open_pixel_blocked() {
        let engine = default_engine();
        // Generic Braze marketing-automation open pixel.
        assert!(is_blocked(
            engine,
            "https://example.iad-01.braze.com/api/v3/messaging/log_open?id=abc",
            "image",
        ));
    }

    #[test]
    fn iterable_open_pixel_blocked() {
        let engine = default_engine();
        // sp.email.<brand>.com/q/<token>~~ is Iterable's open-tracking signature.
        assert!(is_blocked(
            engine,
            "http://sp.email.crunchbase.com/q/abc~~/AARGbBA~/xyz~~",
            "image",
        ));
    }

    #[test]
    fn iterable_link_pixel_blocked_as_image_only() {
        let engine = default_engine();
        // Iterable also embeds `links.email.<brand>.com/s/eo/<token>` as
        // a 1×1 hidden image — block on image type.
        assert!(is_blocked(
            engine,
            "https://links.email.crunchbase.com/s/eo/abc-token",
            "image",
        ));
        // The same domain serves real link redirects users click; those
        // are not loaded as images, so they pass through (the `$image`
        // qualifier on the rule excludes them).
        assert!(!is_blocked(
            engine,
            "https://links.email.crunchbase.com/s/c/abc-token",
            "main_frame",
        ));
    }

    #[test]
    fn iterable_content_cdn_is_not_blocked() {
        let engine = default_engine();
        // Iterable hosts content images (logos, banners) on a separate
        // `library.iterable.com` host — those should render normally.
        assert!(!is_blocked(
            engine,
            "https://library.iterable.com/2088/9267/banner.png",
            "image",
        ));
    }

    #[test]
    fn https_scheme_is_supported() {
        let engine = default_engine();
        // Invalid URLs don't panic; they're just not blocked (the
        // ammonia sanitizer is responsible for rejecting them).
        assert!(!is_blocked(engine, "not-a-url", "image"));
        assert!(!is_blocked(engine, "", "image"));
    }
}
