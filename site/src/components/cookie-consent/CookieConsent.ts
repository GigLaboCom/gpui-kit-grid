// <cookie-consent-banner>: a shadow-DOM banner, after the one on Moat Atlas
// (moa.giglabo.com). Visitors in the EU see it; elsewhere consent is granted
// without asking. The region comes from `/api-shared/ip` on the site's own
// origin, which the cluster routes to the shared service: it answers
// `{ status: 'OK', ip: { isEu: boolean } }` from the visitor's address, so the
// page never sees the address itself. Anything but an explicit `false` (FAIL,
// a 404, a timeout) shows the banner: failing the other way would grant
// consent for Europeans.
import { COOKIE_CONSENT_CSS } from './styles';
import { t } from './translations';
import {
	analyticsActive,
	applyToGtag,
	buildConsentParams,
	denyAll,
	grantAll,
	loadConsent,
	saveConsent,
	trackEvent,
	type ConsentCategories,
} from '../../lib/consent';

const GEO_ENDPOINT = '/api-shared/ip';
/** A real verdict only, for the tab: a failure is retried on the next page. */
const EU_CACHE_KEY = 'gkg-eu-check';
/** The Privacy page's "Cookie settings" button sends this. */
export const OPEN_EVENT = 'cookie-consent:open';

export class CookieConsentBanner extends HTMLElement {
	private customizing = false;
	private categories: ConsentCategories = denyAll();

	constructor() {
		super();
		this.attachShadow({ mode: 'open' });
	}

	connectedCallback() {
		if (!analyticsActive()) return;

		document.addEventListener(OPEN_EVENT, () => {
			this.categories = { ...(loadConsent()?.categories ?? denyAll()) };
			this.customizing = true;
			this.render();
			this.show();
		});

		const stored = loadConsent();
		if (stored) {
			applyToGtag(buildConsentParams(stored.categories));
			return;
		}
		void this.checkAndShow();
	}

	private async checkAndShow() {
		if (await this.needsBanner()) {
			this.render();
			this.show();
		} else {
			const categories = grantAll();
			saveConsent(categories);
			applyToGtag(buildConsentParams(categories));
			trackEvent('cookie_consent', { action: 'auto_grant', region: 'non_eu' });
		}
	}

	private async needsBanner(): Promise<boolean> {
		try {
			const cached = sessionStorage.getItem(EU_CACHE_KEY);
			if (cached !== null) return cached === 'true';
		} catch {
			// No session storage: ask the endpoint every time.
		}
		try {
			const controller = new AbortController();
			const timeout = setTimeout(() => controller.abort(), 3000);
			const res = await fetch(GEO_ENDPOINT, { signal: controller.signal });
			clearTimeout(timeout);
			if (!res.ok) throw new Error(`HTTP ${res.status}`);
			const data = await res.json();
			const isEu = data?.ip?.isEu ?? data?.is_eu;
			if (typeof isEu !== 'boolean') throw new Error('no verdict');
			try {
				sessionStorage.setItem(EU_CACHE_KEY, String(isEu));
			} catch {
				// Not cached; the check runs again on the next page.
			}
			return isEu;
		} catch {
			return true;
		}
	}

	private show() {
		// Two frames, so the hidden position is painted before the transition.
		requestAnimationFrame(() =>
			requestAnimationFrame(() => this.shadowRoot?.querySelector('.banner')?.classList.add('visible')),
		);
	}

	private hide() {
		const banner = this.shadowRoot?.querySelector('.banner');
		if (!banner) return;
		banner.classList.remove('visible');
		banner.addEventListener(
			'transitionend',
			() => {
				if (this.shadowRoot) this.shadowRoot.innerHTML = '';
			},
			{ once: true },
		);
	}

	private commit(categories: ConsentCategories, action: string) {
		saveConsent(categories);
		applyToGtag(buildConsentParams(categories));
		trackEvent('cookie_consent', { action, analytics: categories.analytics, marketing: categories.marketing });
		this.hide();
	}

	private render() {
		const shadow = this.shadowRoot;
		if (!shadow) return;
		shadow.innerHTML = `
			<style>${COOKIE_CONSENT_CSS}</style>
			<div class="banner" role="dialog" aria-label="${t('banner_title')}">
				<div class="banner-inner">
					<p class="banner-title">${t('banner_title')}</p>
					<p class="banner-text">${t('banner_text')}</p>
					<div class="banner-actions">
						<button class="btn btn-accept" data-action="accept">${t('accept_all')}</button>
						<button class="btn btn-reject" data-action="reject">${t('reject_all')}</button>
						<button class="btn btn-customize" data-action="customize">${t('customize')}</button>
						<a class="policy-link" href="/privacy/">${t('policy_link')}</a>
					</div>
					${this.customizing ? this.renderCustomize() : ''}
				</div>
			</div>`;

		shadow.querySelectorAll<HTMLElement>('[data-action]').forEach((btn) => {
			btn.addEventListener('click', () => {
				switch (btn.dataset.action) {
					case 'accept':
						this.commit(grantAll(), 'accept_all');
						break;
					case 'reject':
						this.commit(denyAll(), 'reject_all');
						break;
					case 'save':
						this.commit(this.categories, 'custom_save');
						break;
					case 'customize':
						this.customizing = !this.customizing;
						this.render();
						this.show();
						break;
				}
			});
		});
		shadow.querySelectorAll<HTMLInputElement>('input[data-category]').forEach((input) => {
			input.addEventListener('change', () => {
				const key = input.dataset.category as 'analytics' | 'marketing';
				this.categories[key] = input.checked;
			});
		});
	}

	private renderCustomize(): string {
		const rows = (['essential', 'analytics', 'marketing'] as const).map((key) => {
			const control =
				key === 'essential'
					? `<span class="category-status">${t('always_active')}</span>`
					: `<label class="toggle"><input type="checkbox" data-category="${key}" aria-label="${t(`cat_${key}`)}" ${
							this.categories[key] ? 'checked' : ''
						}><span class="toggle-slider"></span></label>`;
			return `<div class="category">
				<div class="category-info">
					<div class="category-name">${t(`cat_${key}`)}</div>
					<div class="category-desc">${t(`cat_${key}_desc`)}</div>
				</div>${control}</div>`;
		});
		return `<div class="customize-panel">${rows.join('')}
			<div class="banner-actions"><button class="btn btn-accept" data-action="save">${t('save_preferences')}</button></div>
		</div>`;
	}
}
