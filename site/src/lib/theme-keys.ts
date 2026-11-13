// The keys of a theme remix, version 1: the gpui-kit theme file keys the
// embedded examples accept (the web shell's `remix.rs` holds the same
// list, and the Embed page documents it). Keys are only ever added.

/** A colour key under `colors`, and what it paints in the examples. */
export interface ColorKey {
  key: string;
  label: string;
}

export const COLOR_GROUPS: { title: string; keys: ColorKey[] }[] = [
  {
    title: 'Surfaces',
    keys: [
      { key: 'background', label: 'Background' },
      { key: 'foreground', label: 'Text' },
      { key: 'muted.foreground', label: 'Muted text' },
      { key: 'border', label: 'Borders' },
      { key: 'accent.background', label: 'Accent' },
      { key: 'selection.background', label: 'Selection' },
      { key: 'scrollbar.thumb.background', label: 'Scrollbar' },
    ],
  },
  {
    title: 'Table',
    keys: [
      { key: 'table.background', label: 'Rows' },
      { key: 'table.even.background', label: 'Even rows' },
      { key: 'table.head.background', label: 'Header' },
      { key: 'table.head.foreground', label: 'Header text' },
      { key: 'table.hover.background', label: 'Hovered row' },
      { key: 'table.active.background', label: 'Selected row' },
      { key: 'table.active.border', label: 'Selected border' },
      { key: 'table.row.border', label: 'Row lines' },
    ],
  },
  {
    title: 'Meaning',
    keys: [
      { key: 'success.background', label: 'Rise, insert' },
      { key: 'danger.background', label: 'Fall, removal' },
      { key: 'warning.background', label: 'Change' },
      { key: 'chart.bullish', label: 'Up' },
      { key: 'chart.bearish', label: 'Down' },
      { key: 'base.yellow', label: 'Tickers' },
      { key: 'base.cyan', label: 'Totals' },
    ],
  },
];

/** The font families: one of those the examples ship. */
export const FAMILY_KEYS = [
  { key: 'font.family', label: 'Interface font' },
  { key: 'mono_font.family', label: 'Number font' },
];

/** Sizes in pixels (8 to 32), and the radius (0 to 24, whole pixels). */
export const NUMBER_KEYS = [
  { key: 'font.size', label: 'Interface size', min: 8, max: 32, step: 0.5 },
  { key: 'mono_font.size', label: 'Number size', min: 8, max: 32, step: 0.5 },
  { key: 'radius', label: 'Radius', min: 0, max: 24, step: 1 },
];
