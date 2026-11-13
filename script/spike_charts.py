#!/usr/bin/env python3
"""Draw the benchmark charts (SVG) from a `spike_bench` run.

    cargo run --release -p gpui-kit-grid --features test-support --example spike_bench \
        > bench-run.md
    python3 -I script/spike_charts.py bench-run.md bench-charts

    python3 -I script/spike_charts.py <spike_bench output> <output directory>

The input is any file `spike_bench` printed; the raw run itself is not
committed, and neither are the charts.

Writes into the output directory (created if missing):

- `frame-vs-datatable.svg`: S1 against DataTable (S6, S6b);
- `scenarios.svg`: frame p50 and p99 of S1-S4 and S7, the stage 1 frame
  scenarios S1h, S8, S11b, S15, S4b and S12, and under the stage 2 heading
  the stage 2 ones: S20, S16, S16b, S23, S21, S19 (both), S22 and S18 (both);
- `background.svg`: background Rebuild and compaction of the spike and stage
  1, and under the stage 2 heading the grouped, tree and fading rebuilds (S16, S16b,
  S21, S19) and the aggregate and expand jobs (S17, S18): the median or p50
  as a bar, p99 (or the maximum) as a dot, each against its own limit;
- `budgets.svg`: the one-shot scenarios of stage 1 (S13, S11, S14, S9, S10)
  and stage 2 (S9b, S10b, the S17 and S18 jobs, the frames that show S18),
  each as a share of its own limit.

A scenario missing from the run is left out of its chart, and a chart with no
data is not written, so a run of the spike scenarios alone still draws the
three spike charts unchanged.

Input rows, as `spike_bench` prints them:

- frame rows: `| S<id> <description> | draws | draws/s | p50 | p95 | p99 | max |
  overhead | notes |`, the first word of the first cell being the scenario id
  (`S1`, `S3b`, `S1h`, `S11b`);
- measurement rows: `| <description> | <result> |`. A stage 1 scenario starts
  the description with its id, and the result names each number before it, in
  ns, µs, ms or s: `| S9 copy all, 100000 rows × 40 columns | background 312 ms,
  UI thread 41 ms |`. S10 is a frame row that names its accessibility plan
  times in the notes: `plan and nodes p50 0.216 ms, p99 0.249 ms, …`.

The SVGs carry their own light and dark palettes (OS setting, or a `data-theme`
attribute on the page that inlines them) and a `<title>` on every mark for
hover. Colors follow the dataviz reference palette; the two blue steps used
for p50/p99 were checked with its validator in both modes.
"""

import argparse
import math
import re
import sys
from dataclasses import dataclass
from pathlib import Path

STYLE = """
<style>
.sc { --surface:#fcfcfb; --ink:#0b0b0b; --ink2:#52514e; --muted:#898781; --grid:#e1e0d9;
      --axis:#c3c2b7; --s1:#2a78d6; --rest:#bdbcb4; --p50:#86b6ef; --p99:#1c5cab;
      font-family: system-ui, -apple-system, "Segoe UI", sans-serif; }
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) .sc { --surface:#1a1a19; --ink:#f0efec; --ink2:#c3c2b7;
      --grid:#2c2c2a; --axis:#383835; --s1:#3987e5; --rest:#5a5955; --p50:#256abf; --p99:#9ec5f4; }
}
:root[data-theme="dark"] .sc { --surface:#1a1a19; --ink:#f0efec; --ink2:#c3c2b7;
      --grid:#2c2c2a; --axis:#383835; --s1:#3987e5; --rest:#5a5955; --p50:#256abf; --p99:#9ec5f4; }
.sc .bg { fill: var(--surface); }
.sc .title { fill: var(--ink); font-size: 15px; font-weight: 600; }
.sc .sub { fill: var(--ink2); font-size: 12px; }
.sc .label { fill: var(--ink); font-size: 12px; }
.sc .value { fill: var(--ink); font-size: 12px; font-variant-numeric: tabular-nums; }
.sc .tick { fill: var(--muted); font-size: 11px; font-variant-numeric: tabular-nums; }
.sc .note { fill: var(--ink2); font-size: 11px; }
.sc .grid { stroke: var(--grid); stroke-width: 1; }
.sc .axis { stroke: var(--axis); stroke-width: 1; }
.sc .limit { stroke: var(--ink2); stroke-width: 1; }
.sc .s1 { fill: var(--s1); }
.sc .rest { fill: var(--rest); }
.sc .p50 { fill: var(--p50); stroke: var(--surface); stroke-width: 2; }
.sc .p99 { fill: var(--p99); stroke: var(--surface); stroke-width: 2; }
.sc .span { stroke: var(--muted); stroke-width: 2; stroke-linecap: round; }
</style>
"""


def fmt(value: float) -> str:
    """Russian decimal comma: two decimals below 1, none from 100, else one."""
    if value >= 100:
        return f"{value:.0f}"
    text = f"{value:.2f}" if value < 1 else f"{value:.1f}"
    return text.replace(".", ",")


def esc(text: str) -> str:
    return text.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def bar(x: float, y: float, w: float, h: float, cls: str, title: str, rounded: bool) -> str:
    """A bar growing right from x: square at the baseline, 4px round data-end."""
    if w <= 0:
        return ""
    r = min(4.0, w / 2, h / 2) if rounded else 0.0
    path = (
        f"M{x:.1f},{y:.1f} H{x + w - r:.1f} "
        f"Q{x + w:.1f},{y:.1f} {x + w:.1f},{y + r:.1f} V{y + h - r:.1f} "
        f"Q{x + w:.1f},{y + h:.1f} {x + w - r:.1f},{y + h:.1f} H{x:.1f} Z"
    )
    return f'<path class="{cls}" d="{path}"><title>{esc(title)}</title></path>'


def frame(width: int, height: int, title: str, subtitle: str, body: str, label: str) -> str:
    return (
        f'<svg class="sc" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" '
        f'width="{width}" height="{height}" role="img" aria-label="{esc(label)}">'
        f"{STYLE}"
        f'<rect class="bg" x="0" y="0" width="{width}" height="{height}" rx="8"/>'
        f'<text class="title" x="20" y="30">{esc(title)}</text>'
        f'<text class="sub" x="20" y="50">{esc(subtitle)}</text>'
        f"{body}</svg>\n"
    )


def x_axis(x0: float, x1: float, top: float, bottom: float, max_value: float, step: float, unit: str) -> str:
    parts = []
    value = 0.0
    while value <= max_value + 1e-9:
        x = x0 + (x1 - x0) * value / max_value
        parts.append(f'<line class="grid" x1="{x:.1f}" y1="{top}" x2="{x:.1f}" y2="{bottom}"/>')
        tick = f"{value:g}".replace(".", ",")
        parts.append(f'<text class="tick" x="{x:.1f}" y="{bottom + 16}" text-anchor="middle">{tick}</text>')
        value += step
    parts.append(f'<line class="axis" x1="{x0}" y1="{top}" x2="{x0}" y2="{bottom}"/>')
    parts.append(f'<text class="tick" x="{x1:.1f}" y="{bottom + 32}" text-anchor="end">{esc(unit)}</text>')
    return "".join(parts)


def legend(x: float, y: float, items: list[tuple[str, str, str]]) -> str:
    """items: (css class, shape 'rect'|'dot', text)."""
    parts = []
    for cls, shape, text in items:
        if shape == "dot":
            parts.append(f'<circle class="{cls}" cx="{x + 6}" cy="{y - 4}" r="5"/>')
        else:
            parts.append(f'<rect class="{cls}" x="{x}" y="{y - 10}" width="12" height="12" rx="3"/>')
        parts.append(f'<text class="label" x="{x + 18}" y="{y}">{esc(text)}</text>')
        x += 26 + 7 * len(text)
    return "".join(parts)


SCENARIO_ID = re.compile(r"(S\d+[a-z]?) ")
DURATION = r"(\d+(?:\.\d+)?)\s*(ns|µs|us|ms|s)\b"
MS_PER_UNIT = {"ns": 1e-6, "µs": 1e-3, "us": 1e-3, "ms": 1.0, "s": 1000.0}
# Columns of a frame row, by index.
FRAME_COLUMNS = {3: "p50", 4: "p95", 5: "p99", 6: "max"}
NOTES_COLUMN = 8


def warn(text: str) -> None:
    print(f"warning: {text}", file=sys.stderr)


@dataclass
class Run:
    """The tables of one `spike_bench` run."""

    # Frame rows by scenario id, in input order.
    frames: dict[str, list[list[str]]]
    # Measurement rows as (description, result), in input order.
    measurements: list[tuple[str, str]]

    def frame(self, key: str, variant: str | None = None) -> list[str] | None:
        """The first frame row of a scenario; later rows with its id are
        variants, picked by a regex on the description."""
        rows = self.frames.get(key) or []
        if variant is not None:
            rows = [cells for cells in rows if re.search(variant, cells[0])]
        return rows[0] if rows else None

    def measurement(self, prefix: str) -> str | None:
        """The result of the first measurement whose description starts with `prefix`."""
        return next((result for name, result in self.measurements if name.startswith(prefix)), None)

    def scenario_measurements(self, key: str) -> list[tuple[str, str]]:
        return [(name, result) for name, result in self.measurements if scenario_id(name) == key]

    def has(self, key: str) -> bool:
        return key in self.frames or bool(self.scenario_measurements(key))


def scenario_id(text: str) -> str | None:
    match = SCENARIO_ID.match(text)
    return match.group(1) if match else None


def parse(path: Path) -> Run:
    run = Run(frames={}, measurements=[])
    for line in path.read_text(encoding="utf-8").splitlines():
        if not line.startswith("| ") or line.startswith("|---"):
            continue
        cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
        key = scenario_id(cells[0])
        if len(cells) == 2:
            run.measurements.append((cells[0], cells[1]))
        elif key and len(cells) >= 9:
            run.frames.setdefault(key, []).append(cells)
    return run


def number(text: str) -> float:
    return float(re.search(r"[\d.]+", text).group(0))


def try_number(text: str) -> float | None:
    match = re.search(r"\d+(?:\.\d+)?", text)
    return float(match.group(0)) if match else None


def labelled(text: str, labels: tuple[str, ...]) -> tuple[float, str] | None:
    """The first duration named by one of `labels` (regex), in ms, and its label."""
    for label in labels:
        match = re.search(rf"\b({label})\b[^\d,;]*?{DURATION}", text)
        if match:
            return float(match.group(2)) * MS_PER_UNIT[match.group(3)], match.group(1)
    return None


def bare(text: str) -> tuple[float, str] | None:
    """The first duration in `text`, in ms, for a result with one unnamed number."""
    match = re.search(DURATION, text)
    return (float(match.group(1)) * MS_PER_UNIT[match.group(2)], "") if match else None


def nice_axis(value: float, minimum: float, steps: tuple[float, ...]) -> tuple[float, float]:
    """An axis maximum of at least `minimum` that holds `value`, and its tick step."""
    top = max(value, minimum)
    step = next((s for s in steps if top / s <= 8), steps[-1])
    return max(minimum, math.ceil(top / step - 1e-9) * step), step


def chart_frame_vs_datatable(run: Run) -> str | None:
    s = {key: run.frame(key) for key in ("S1", "S6", "S6b")}
    if any(cells is None for cells in s.values()):
        return None
    # Columns: name, draws, draws/s, p50, p95, p99, max, overhead, notes.
    rows = [
        ("gpui-kit-grid", number(s["S1"][3]), number(s["S1"][7])),
        ("DataTable, быстрый делегат", number(s["S6b"][3]), number(s["S6b"][7])),
        ("DataTable, типичный делегат", number(s["S6"][3]), number(s["S6"][7])),
    ]
    width, left, right = 760, 230, 680
    top, row_h, bar_h = 92, 40, 18
    bottom = top + row_h * len(rows)
    max_value = 16.0
    scale = (right - left) / max_value
    body = [x_axis(left, right, top - 8, bottom, max_value, 4, "мс на кадр, p50")]
    for ix, (name, subtree, overhead) in enumerate(rows):
        y = top + ix * row_h + (row_h - bar_h) / 2
        w1, w2 = subtree * scale, overhead * scale
        body.append(f'<text class="label" x="{left - 12}" y="{y + bar_h / 2 + 4}" text-anchor="end">{esc(name)}</text>')
        body.append(bar(left, y, w1, bar_h, "s1", f"{name}: компонент {fmt(subtree)} мс", False))
        # 2px surface gap between stacked segments.
        body.append(bar(left + w1 + 2, y, max(w2 - 2, 1), bar_h, "rest", f"{name}: GPUI вне компонента {fmt(overhead)} мс", True))
        total = subtree + overhead
        body.append(f'<text class="value" x="{left + w1 + w2 + 8}" y="{y + bar_h / 2 + 4}">{fmt(total)} мс</text>')
    ratio_fast = (rows[1][1] + rows[1][2]) / (rows[0][1] + rows[0][2])
    body.append(legend(left, bottom + 54, [("s1", "rect", "Отрисовка компонента"), ("rest", "rect", "Работа GPUI вне компонента")]))
    return frame(
        width,
        bottom + 74,
        f"Кадр: gpui-kit-grid в {ratio_fast:.0f} раз быстрее DataTable",
        "Нагрузка S1: 2k обновлений/с в видимых строках, 2560×1440 при 2×, ~1500 ячеек",
        "".join(body),
        "Время кадра gpui-kit-grid и DataTable",
    )


@dataclass(frozen=True)
class FrameScenario:
    """A frame scenario of the chart: its id, label and p99 limit in ms."""

    key: str
    name: str
    limit: float
    # A regex on the description that picks one of the scenario's rows.
    variant: str | None = None
    # The stage that added it: stage 2 rows go under their own label.
    stage: int = 1


# Spike scenarios first in each limit group; S1h, S8, S11b, S15, S4b and S12
# come from stage 1, the rest from stage 2.
FRAME_SCENARIOS = [
    FrameScenario("S1", "Обновления в видимых строках, 2k/с", 8),
    FrameScenario("S1h", "S1 + курсор над телом и тултип", 8),
    FrameScenario("S2", "P2: 10k/с, сортировка по меняющейся колонке", 8),
    FrameScenario("S7", "P2 + удаления и вставки по 1k/с", 8),
    FrameScenario("S8", "P2 + полосы и неопределённый прогресс", 8),
    FrameScenario("S11b", "P2 + набор текста в быстром фильтре", 8),
    FrameScenario("S3", "Быстрая прокрутка: новая страница в кадр", 12),
    FrameScenario("S3b", "Горизонтальная прокрутка", 12),
    FrameScenario("S15", "P2: PageDown с автоповтором 30 Гц", 12),
    FrameScenario("S4", "Ресайз двух видимых колонок", 12),
    FrameScenario("S4b", "Ресайз колонки правой зоны", 12),
    FrameScenario("S12", "Перетаскивание заголовка с автопрокруткой", 12),
    FrameScenario("S20", "S1 со стрелками направления", 8, stage=2),
    FrameScenario("S16", "P2 по 1 колонке, Sum и Avg", 12, r"all expanded$", 2),
    FrameScenario("S16b", "P2 по 2 колонкам, половина свёрнута", 12, stage=2),
    FrameScenario("S23", "S16 + футеры групп и итоги", 12, stage=2),
    FrameScenario("S21", "P2 по 1 колонке + угасание удалённых", 12, stage=2),
    FrameScenario("S19", "Дерево 100k, быстрый фильтр", 12, "quick filter", 2),
    FrameScenario("S19", "Дерево 100k, всё раскрыто", 12, "all expanded", 2),
    FrameScenario("S22", "Заголовок через область группировки", 12, stage=2),
    FrameScenario("S18", "Раскрытие группы на 50k строк", 12, "group of", 2),
    FrameScenario("S18", "Раскрыть и свернуть всё, 1000 групп", 12, "1000 groups", 2),
]

# The label of the stage 2 rows of a chart.
STAGE_2 = "Этап 2: группы, дерево, живые изменения"


def stage_rows(rows: list[tuple], stage_of) -> list[tuple | None]:
    """`rows` with `None` before the first stage 2 row: a label line."""
    out: list[tuple | None] = []
    for row in rows:
        if stage_of(row) == 2 and None not in out:
            out.append(None)
        out.append(row)
    return out


def chart_scenarios(run: Run) -> str | None:
    rows = []
    for scenario in FRAME_SCENARIOS:
        cells = run.frame(scenario.key, scenario.variant)
        if cells is None:
            continue
        p50, p99 = try_number(cells[3]), try_number(cells[5])
        if p50 is None or p99 is None:
            warn(f"{scenario.key}: no p50 or p99 in its frame row")
            continue
        rows.append((scenario.key, scenario.name, scenario.limit, p50, p99, scenario.stage))
    if not rows:
        return None
    lines = stage_rows(rows, lambda row: row[5])
    width, left, right = 760, 300, 700
    top, row_h = 92, 34
    bottom = top + row_h * len(lines)
    max_value, step = nice_axis(max(row[4] for row in rows), 12.0, (2, 4, 5, 10, 20, 50))
    scale = (right - left) / max_value
    body = [x_axis(left, right, top - 8, bottom, max_value, step, "мс на кадр")]
    for ix, line in enumerate(lines):
        cy = top + ix * row_h + row_h / 2
        if line is None:
            body.append(f'<text class="title" style="font-size:13px" x="20" y="{cy + 5}">{esc(STAGE_2)}</text>')
            continue
        key, name, limit, p50, p99, _ = line
        body.append(f'<text class="label" x="{left - 12}" y="{cy + 4}" text-anchor="end">{esc(name)}</text>')
        lx = left + limit * scale
        body.append(f'<line class="limit" x1="{lx:.1f}" y1="{cy - 9}" x2="{lx:.1f}" y2="{cy + 9}"><title>Порог p99: {limit} мс</title></line>')
        x50, x99 = left + p50 * scale, left + p99 * scale
        body.append(f'<line class="span" x1="{x50:.1f}" y1="{cy}" x2="{x99:.1f}" y2="{cy}"/>')
        body.append(f'<circle class="p50" cx="{x50:.1f}" cy="{cy}" r="5"><title>{key} p50: {fmt(p50)} мс</title></circle>')
        body.append(f'<circle class="p99" cx="{x99:.1f}" cy="{cy}" r="5"><title>{key} p99: {fmt(p99)} мс</title></circle>')
        body.append(f'<text class="value" x="{x99 + 10:.1f}" y="{cy + 4}">{fmt(p99)}</text>')
    body.append(legend(left, bottom + 54, [("p50", "dot", "p50"), ("p99", "dot", "p99")]))
    body.append(f'<line class="limit" x1="{left + 120}" y1="{bottom + 44}" x2="{left + 120}" y2="{bottom + 56}"/>')
    body.append(f'<text class="label" x="{left + 128}" y="{bottom + 54}">порог p99 (8 или 12 мс)</text>')
    return frame(
        width,
        bottom + 74,
        "Кадр gpui-kit-grid по сценариям: p50 и p99",
        "Каждая настоящая отрисовка компонента, 300 кадров; 100k строк × 40 колонок",
        "".join(body),
        "p50 и p99 времени кадра по сценариям",
    )


@dataclass(frozen=True)
class BackgroundJob:
    """A background measurement of the chart: the median or p50 as a bar, the
    p99 (or the maximum) as a dot, against the job's own limit."""

    name: str
    prefix: str
    limit: float | None
    stage: int = 1


BACKGROUND_JOBS = [
    BackgroundJob("Порядок вставки", "Rebuild 100000 rows, insertion", None),
    BackgroundJob("Сортировка по числу", "Rebuild 100000 rows, float", 30),
    BackgroundJob("Сортировка по тексту", "Rebuild 100000 rows, text column", 30),
    BackgroundJob("Текст + число + фильтр", "Rebuild 100000 rows, text +", 30),
    BackgroundJob("Сжатие после 1k удалений", "after 1k removals", None),
    BackgroundJob("S16: группы по 1 колонке, Sum и Avg", "S16 Rebuild 100000", 30, 2),
    BackgroundJob("S16b: 2 колонки, 600 групп", "S16b Rebuild", 30, 2),
    BackgroundJob("S21: S16 и 1000 угасающих строк", "S21 Rebuild", 30, 2),
    BackgroundJob("S19: дерево 100k, быстрый фильтр", "S19 tree", 40, 2),
    BackgroundJob("S17: задача агрегатов", "S17 aggregate", 10, 2),
    BackgroundJob("S18: раскрыть группу на 50k", "S18 expand job: expand and collapse", 10, 2),
]


def job_values(result: str) -> tuple[float, float | None]:
    """The median (or p50) of a background result and its p99 (or maximum);
    the compaction row (`0.27 ms vs 3.1 ms`) has the first number only."""
    middle = labelled(result, ("median", "p50"))
    worst = labelled(result, ("p99", "max"))
    if middle is None:
        return float(re.findall(r"[\d.]+", result)[0]), None
    return middle[0], worst[0] if worst else None


def chart_background(run: Run) -> str | None:
    rows = []
    for job in BACKGROUND_JOBS:
        result = run.measurement(job.prefix)
        if result is not None:
            rows.append((job, *job_values(result)))
    if not rows:
        return None
    lines = stage_rows(rows, lambda row: row[0].stage)
    width, left, right = 760, 260, 680
    top, row_h, bar_h = 92, 34, 16
    bottom = top + row_h * len(lines)
    largest = max(max(value, worst or 0, job.limit or 0) for job, value, worst in rows)
    max_value, step = nice_axis(largest, 32.0, (4, 8, 10, 20, 50))
    scale = (right - left) / max_value
    body = [x_axis(left, right, top - 8, bottom, max_value, step, "мс, фоновый поток")]
    for ix, line in enumerate(lines):
        y = top + ix * row_h + (row_h - bar_h) / 2
        cy = y + bar_h / 2
        if line is None:
            body.append(f'<text class="title" style="font-size:13px" x="20" y="{cy + 5}">{esc(STAGE_2)}</text>')
            continue
        job, value, worst = line
        body.append(f'<text class="label" x="{left - 12}" y="{cy + 4}" text-anchor="end">{esc(job.name)}</text>')
        body.append(bar(left, y, max(value * scale, 2), bar_h, "s1", f"{job.name}: медиана {fmt(value)} мс", True))
        end = left + max(value * scale, 2)
        if worst is not None:
            wx = left + worst * scale
            body.append(f'<circle class="p99" cx="{wx:.1f}" cy="{cy}" r="5"><title>{esc(job.name)}: p99 или максимум {fmt(worst)} мс</title></circle>')
            end = max(end, wx + 4)
        if job.limit is not None:
            lx = left + job.limit * scale
            body.append(f'<line class="limit" x1="{lx:.1f}" y1="{cy - 11}" x2="{lx:.1f}" y2="{cy + 11}"><title>Порог: {fmt(job.limit)} мс</title></line>')
        body.append(f'<text class="value" x="{end + 8:.1f}" y="{cy + 4}">{fmt(value)} мс</text>')
    body.append(legend(left, bottom + 54, [("s1", "rect", "медиана"), ("p99", "dot", "p99 или максимум")]))
    body.append(f'<line class="limit" x1="{left + 280}" y1="{bottom + 44}" x2="{left + 280}" y2="{bottom + 56}"/>')
    body.append(f'<text class="label" x="{left + 288}" y="{bottom + 54}">порог задачи</text>')
    return frame(
        width,
        bottom + 74,
        "Фоновые задачи проекции, 100k строк",
        "Полный Rebuild сортировок, групп и дерева, сжатие, задачи агрегатов и раскрытия",
        "".join(body),
        "Время фоновых задач проекции",
    )


@dataclass(frozen=True)
class Budget:
    """A one-shot measurement of a stage 1 scenario against its limit."""

    key: str
    name: str
    limit: float
    # Result labels (regex) tried in order, e.g. `p99` in `p50 0.21 ms, p99 0.30 ms`.
    labels: tuple[str, ...]
    # Column read when the scenario prints frame rows instead (`FRAME_COLUMNS`).
    frame_column: int | None = None
    # Whether the labels are read in the notes of the frame rows instead: the
    # frame times are not what the scenario limits.
    frame_notes: bool = False
    # Whether an unlabelled duration counts when no label matches.
    fallback: bool = True
    # Rows whose description matches win over the other rows of the scenario.
    prefer: str | None = None
    # The stage that added it: stage 2 rows go under their own label.
    stage: int = 1


BUDGETS = [
    Budget("S13", "«Подобрать все», один кадр", 16, ("max", "p99"), frame_column=6),
    Budget("S11", "Быстрый фильтр P2: Rebuild", 30, ("p99", "max", "median")),
    Budget("S14", "Выделить всё P2, ключи u64", 16, ("p99", "max", "median"), frame_column=5, prefer="u64"),
    Budget("S9", "«Copy all» 100k строк: UI-поток", 50, (r"UI[ -]thread", "UI"), fallback=False),
    Budget("S9", "«Copy all» 100k строк: фон", 500, ("background",), fallback=False),
    Budget("S10", "План и узлы доступности", 1, ("p99",), frame_notes=True),
    # Stage 2.
    Budget("S9b", "«Copy all» всё раскрыто: UI-поток", 50, (r"UI[ -]thread", "UI"), fallback=False, stage=2),
    Budget("S9b", "«Copy all» всё раскрыто: фон", 500, ("background",), fallback=False, stage=2),
    Budget("S10b", "План доступности с заголовками", 1, ("p99",), frame_notes=True, stage=2),
    Budget("S17", "Задача агрегатов, 1k изменений", 10, ("p99",), stage=2),
    Budget("S18", "Задача раскрытия", 10, ("p99",), stage=2),
    Budget("S18", "Кадр, показывающий раскрытие", 12, ("max",), frame_column=6, stage=2),
]


def budget_value(run: Run, budget: Budget) -> tuple[float, str] | None:
    """The worst value of a scenario's rows (preferred rows only, if any), and its label."""
    found: list[tuple[bool, float, str]] = []
    for cells in run.frames.get(budget.key, []):
        if budget.frame_notes:
            value = labelled(cells[NOTES_COLUMN], budget.labels)
            if value is not None:
                found.append((bool(budget.prefer and re.search(budget.prefer, cells[0])), *value))
        elif budget.frame_column is not None:
            value = try_number(cells[budget.frame_column])
            if value is not None:
                preferred = bool(budget.prefer and re.search(budget.prefer, cells[0]))
                found.append((preferred, value, FRAME_COLUMNS[budget.frame_column]))
    for name, result in run.scenario_measurements(budget.key):
        value = labelled(f"{name} | {result}", budget.labels) or (bare(result) if budget.fallback else None)
        if value is not None:
            preferred = bool(budget.prefer and re.search(budget.prefer, name))
            found.append((preferred, *value))
    candidates = [item for item in found if item[0]] or found
    if not candidates:
        return None
    _, value, label = max(candidates, key=lambda item: item[1])
    return value, label


def chart_budgets(run: Run) -> str | None:
    rows = []
    for budget in BUDGETS:
        if not run.has(budget.key):
            continue
        value = budget_value(run, budget)
        if value is None:
            warn(f"{budget.key}: no value for «{budget.name}» (labels {', '.join(budget.labels)})")
            continue
        rows.append((budget, *value))
    if not rows:
        return None
    lines = stage_rows(rows, lambda row: row[0].stage)
    width, left, right = 760, 260, 620
    top, row_h, bar_h = 92, 34, 16
    bottom = top + row_h * len(lines)
    max_share = max(100 * value / budget.limit for budget, value, _ in rows)
    max_value, step = nice_axis(max_share, 100.0, (25, 50, 100, 250, 500, 1000))
    scale = (right - left) / max_value
    body = [x_axis(left, right, top - 8, bottom, max_value, step, "% порога сценария")]
    for ix, line in enumerate(lines):
        y = top + ix * row_h + (row_h - bar_h) / 2
        if line is None:
            body.append(f'<text class="title" style="font-size:13px" x="20" y="{y + bar_h / 2 + 5}">{esc(STAGE_2)}</text>')
            continue
        budget, value, label = line
        share = 100 * value / budget.limit
        w = max(share * scale, 2)
        limit = f"{budget.limit:g}".replace(".", ",")
        metric = f" {label}" if label else ""
        title = f"{budget.key}{metric}: {fmt(value)} мс, порог {limit} мс ({share:.0f}%)"
        body.append(f'<text class="label" x="{left - 12}" y="{y + bar_h / 2 + 4}" text-anchor="end">{esc(budget.name)}</text>')
        body.append(bar(left, y, w, bar_h, "s1", title, True))
        body.append(f'<text class="value" x="{left + w + 8:.1f}" y="{y + bar_h / 2 + 4}">{fmt(value)} из {limit} мс</text>')
    lx = left + 100 * scale
    body.append(f'<line class="limit" x1="{lx:.1f}" y1="{top - 8}" x2="{lx:.1f}" y2="{bottom}"><title>Порог сценария: 100%</title></line>')
    body.append(f'<text class="note" x="{lx - 6:.1f}" y="{top - 14}" text-anchor="end">порог</text>')
    return frame(
        width,
        bottom + 54,
        "Разовые операции и задачи: доля порога",
        "Время операции, делённое на порог её сценария; худший из вариантов сценария",
        "".join(body),
        "Время разовых операций в долях их порогов",
    )


# Scenarios the charts draw, for the report of what a run lacks.
KNOWN = (
    ["S1", "S6", "S6b"]
    + [scenario.key for scenario in FRAME_SCENARIOS]
    + [budget.key for budget in BUDGETS]
)


def main() -> None:
    parser = argparse.ArgumentParser(description="Draw the benchmark charts (SVG) from a spike_bench run.")
    parser.add_argument("run", type=Path, help="a file spike_bench printed, e.g. bench-run.md")
    parser.add_argument("out", type=Path, help="directory for the SVGs (created if missing)")
    args = parser.parse_args()
    if not args.run.is_file():
        parser.error(f"no such file: {args.run}")
    if args.out.exists() and not args.out.is_dir():
        parser.error(f"not a directory: {args.out}")
    run = parse(args.run)
    charts = {
        "frame-vs-datatable.svg": chart_frame_vs_datatable(run),
        "scenarios.svg": chart_scenarios(run),
        "background.svg": chart_background(run),
        "budgets.svg": chart_budgets(run),
    }
    for name, svg in charts.items():
        if svg is None:
            print(f"skipped {args.out / name}: none of its rows are in the run")
            continue
        args.out.mkdir(parents=True, exist_ok=True)
        (args.out / name).write_text(svg, encoding="utf-8")
        print(f"wrote {args.out / name}")
    missing = [key for key in dict.fromkeys(KNOWN) if not run.has(key)]
    if missing:
        print(f"not in the run: {', '.join(missing)}")


if __name__ == "__main__":
    main()
