// The banner's styles, inside its shadow root. Custom properties inherit into
// it, so the colours follow Starlight's light and dark themes.
export const COOKIE_CONSENT_CSS = `
:host {
  all: initial;
  font-family: var(--sl-font, system-ui, sans-serif);
  color: var(--sl-color-white);
  line-height: 1.5;
}
*, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }

.banner {
  position: fixed;
  inset: auto 0 0 0;
  z-index: 999999;
  background: var(--sl-color-bg-nav);
  border-top: 1px solid var(--sl-color-hairline-shade);
  box-shadow: 0 -4px 24px rgba(0, 0, 0, 0.25);
  padding: 20px 24px;
  transform: translateY(100%);
  transition: transform 0.4s cubic-bezier(0.16, 1, 0.3, 1);
}
.banner.visible { transform: translateY(0); }
@media (prefers-reduced-motion: reduce) { .banner { transition: none; } }
.banner-inner { max-width: 1100px; margin: 0 auto; }
.banner-title { font-size: 16px; font-weight: 600; margin-bottom: 6px; }
.banner-text { font-size: 14px; color: var(--sl-color-gray-2); margin-bottom: 16px; max-width: 720px; }
.banner-actions { display: flex; flex-wrap: wrap; align-items: center; gap: 10px; }

.btn {
  font: inherit;
  font-size: 14px;
  padding: 8px 16px;
  border-radius: 6px;
  border: 1px solid var(--sl-color-gray-5);
  background: transparent;
  color: var(--sl-color-white);
  cursor: pointer;
  white-space: nowrap;
}
.btn:hover { border-color: var(--sl-color-accent); }
.btn:focus-visible, .policy-link:focus-visible { outline: 2px solid var(--sl-color-accent); outline-offset: 2px; }
.btn-accept { background: var(--sl-color-accent); border-color: var(--sl-color-accent); color: var(--sl-color-black); }
.btn-accept:hover { background: var(--sl-color-accent-high); border-color: var(--sl-color-accent-high); }
.btn-customize { border-color: transparent; color: var(--sl-color-gray-2); text-decoration: underline; text-underline-offset: 3px; }
.policy-link { margin-left: auto; font-size: 13px; color: var(--sl-color-gray-2); }
.policy-link:hover { color: var(--sl-color-accent); }

.customize-panel { margin-top: 16px; padding-top: 8px; border-top: 1px solid var(--sl-color-hairline); }
.customize-panel .banner-actions { margin-top: 12px; }
.category { display: flex; align-items: center; justify-content: space-between; gap: 16px; padding: 10px 0; border-bottom: 1px solid var(--sl-color-hairline); }
.category-name { font-size: 14px; font-weight: 600; }
.category-desc { font-size: 13px; color: var(--sl-color-gray-3); }
.category-status { font-size: 12px; color: var(--sl-color-gray-3); white-space: nowrap; }

.toggle { position: relative; width: 44px; height: 24px; flex-shrink: 0; }
.toggle input { opacity: 0; width: 0; height: 0; }
.toggle-slider { position: absolute; inset: 0; background: var(--sl-color-gray-5); border-radius: 12px; cursor: pointer; transition: background 0.2s; }
.toggle-slider::before { content: ''; position: absolute; width: 18px; height: 18px; left: 3px; bottom: 3px; background: var(--sl-color-white); border-radius: 50%; transition: transform 0.2s; }
.toggle input:checked + .toggle-slider { background: var(--sl-color-accent); }
.toggle input:checked + .toggle-slider::before { transform: translateX(20px); }
.toggle input:focus-visible + .toggle-slider { outline: 2px solid var(--sl-color-accent); outline-offset: 2px; }

@media (max-width: 640px) {
  .banner { padding: 16px; }
  .banner-actions { flex-direction: column; align-items: stretch; }
  .policy-link { margin-left: 0; text-align: center; }
}
`;
