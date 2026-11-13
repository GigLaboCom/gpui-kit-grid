// The visitor's cookie choice and Google Consent Mode v2, after the banner of
// the sibling site Moat Atlas (moa.giglabo.com). Everything starts denied
// (`consentHead` below, in the page's head): Google Analytics runs without
// cookies until the visitor accepts. The choice is the only thing the banner
// stores, in `gkg-consent`; the Privacy page lists it.

/** Where analytics and the banner run: the published site, nowhere else. */
export const ANALYTICS_HOST = 'gkg.giglabo.com';
/** The site's Google Tag Manager container. */
export const GTM_ID = 'GTM-PVBZHHL9';
export const CONSENT_KEY = 'gkg-consent';
const CONSENT_VERSION = 1;

export interface ConsentCategories {
	essential: true;
	analytics: boolean;
	marketing: boolean;
}

export interface ConsentState {
	categories: ConsentCategories;
	timestamp: number;
	version: number;
}

type Grant = 'granted' | 'denied';
export interface ConsentParams {
	ad_storage: Grant;
	ad_user_data: Grant;
	ad_personalization: Grant;
	analytics_storage: Grant;
	functionality_storage: Grant;
	personalization_storage: Grant;
	security_storage: Grant;
}

declare global {
	interface Window {
		gtag?: (...args: unknown[]) => void;
		dataLayer?: unknown[];
	}
}

/** True on the published site's own pages, not in a frame or another copy. */
export function analyticsActive(): boolean {
	return location.hostname === ANALYTICS_HOST && window.top === window;
}

export function loadConsent(): ConsentState | null {
	try {
		const raw = localStorage.getItem(CONSENT_KEY);
		if (!raw) return null;
		const parsed = JSON.parse(raw) as ConsentState;
		return parsed.version === CONSENT_VERSION ? parsed : null;
	} catch {
		return null;
	}
}

export function saveConsent(categories: ConsentCategories): void {
	const state: ConsentState = { categories, timestamp: Date.now(), version: CONSENT_VERSION };
	try {
		localStorage.setItem(CONSENT_KEY, JSON.stringify(state));
	} catch {
		// Storage blocked: the choice holds for this page only.
	}
}

export function buildConsentParams(c: ConsentCategories): ConsentParams {
	const g = (on: boolean): Grant => (on ? 'granted' : 'denied');
	return {
		analytics_storage: g(c.analytics),
		ad_storage: g(c.marketing),
		ad_user_data: g(c.marketing),
		ad_personalization: g(c.marketing),
		functionality_storage: 'denied',
		personalization_storage: 'denied',
		security_storage: 'granted',
	};
}

export function grantAll(): ConsentCategories {
	return { essential: true, analytics: true, marketing: true };
}

export function denyAll(): ConsentCategories {
	return { essential: true, analytics: false, marketing: false };
}

export function applyToGtag(params: ConsentParams): void {
	window.gtag?.('consent', 'update', params);
}

export function trackEvent(name: string, params: Record<string, unknown> = {}): void {
	window.gtag?.('event', name, params);
}

/**
 * The inline script for the page's head (astro.config.mjs): Consent Mode
 * defaults, a stored choice applied before the container starts, then GTM.
 * Only on the published site (`ANALYTICS_HOST`), never in a frame, so a copy
 * of the site image somewhere else sends nothing.
 */
export function consentHead(): string {
	return `(function () {
  if (location.hostname !== ${JSON.stringify(ANALYTICS_HOST)} || window.top !== window) return;
  window.dataLayer = window.dataLayer || [];
  function gtag() { dataLayer.push(arguments); }
  window.gtag = gtag;
  gtag('consent', 'default', {
    ad_storage: 'denied', ad_user_data: 'denied', ad_personalization: 'denied',
    analytics_storage: 'denied', functionality_storage: 'denied',
    personalization_storage: 'denied', security_storage: 'granted',
    wait_for_update: 500
  });
  try {
    var s = JSON.parse(localStorage.getItem(${JSON.stringify(CONSENT_KEY)}) || 'null');
    if (s && s.version === ${CONSENT_VERSION} && s.categories) {
      var a = s.categories.analytics ? 'granted' : 'denied';
      var m = s.categories.marketing ? 'granted' : 'denied';
      gtag('consent', 'update', { analytics_storage: a, ad_storage: m, ad_user_data: m, ad_personalization: m });
    }
  } catch (e) {}
  dataLayer.push({ 'gtm.start': new Date().getTime(), event: 'gtm.js' });
  var j = document.createElement('script');
  j.async = true;
  j.src = 'https://www.googletagmanager.com/gtm.js?id=' + ${JSON.stringify(GTM_ID)};
  document.head.appendChild(j);
})();`;
}
