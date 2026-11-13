// An example's page opened with `?embed`, usually in another site's iframe
// (`<live-grid mode="frame">` makes one): no switcher of examples, the
// canvas over the frame but for a line with the example's notice and a link
// to the licences, and the guest's manners (`guest.js`). The page around
// the frame can change the theme by message:
//
//   frame.contentWindow.postMessage({ type: 'live-grid:set-theme', id, theme }, '*');
//
// `theme` is a remix (an object, JSON or `v1.…`); the page answers
// `{ type: 'live-grid:theme', id, report }` or `{ …, error }`, and says
// `{ type: 'live-grid:ready' }` once the example is on screen. Only the
// frame's parent is listened to.

import { LINE_HEIGHT, addGuest, creditLine, holdFocus } from './guest.js';
import { applyTheme } from './theme.js';

// The switcher goes; the line under the canvas keeps what it said about
// the example: its notice and the licences.
const STYLE = `
  #apps { display: none; }
  body > canvas { top: 0 !important; height: calc(100% - ${LINE_HEIGHT}px) !important; }
`;

/// Before the module runs: the page's look, the line with the page's
/// `notice` (the markets' "Simulated prices, not market data") and the
/// link to the `licenses` page, and the keyboard held.
export function beforeRun({ notice, licenses }) {
  const style = document.createElement('style');
  style.textContent = STYLE;
  document.head.append(style);
  const line = creditLine({ notice, licenses });
  Object.assign(line.style, { position: 'fixed', left: '0', right: '0', bottom: '0' });
  document.body.append(line);
  holdFocus();
}

/// Once the example is on screen (`fontUrl` turns a path under `fonts/`
/// into its address).
export function afterRun(wasm, fontUrl) {
  addGuest({
    root: document.body,
    events: window,
    canvas: () => document.querySelector('body > canvas'),
    wasm,
  });
  if (window.parent === window) return;
  addEventListener('message', async (event) => {
    const message = event.data;
    if (event.source !== window.parent || message?.type !== 'live-grid:set-theme') return;
    const reply = { type: 'live-grid:theme', id: message.id };
    try {
      reply.report = await applyTheme(wasm, message.theme, fontUrl);
    } catch (error) {
      reply.error = String(error.message ?? error);
    }
    event.source.postMessage(reply, '*');
  });
  window.parent.postMessage({ type: 'live-grid:ready' }, '*');
}
