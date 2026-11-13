#!/usr/bin/env python3
"""Behavioural end-to-end run of `blotter`, `portfolio` and `dock_layout`
on Linux.

xdotool drives a real X11 window with the mouse and the keyboard, through
the X server, GPUI's X11 backend and the grid. `blotter` reports what
happened in `BLOTTER_E2E_LOG` (examples/blotter/e2e.rs): every grid event, a
state snapshot after each, the changes its style hooks saw painted, and
where the header cells, rows and toolbar buttons are. `portfolio` does the
same in `PORTFOLIO_E2E_LOG` (examples/portfolio/e2e.rs), `dock_layout` its
grids in `DOCK_E2E_LOG`. This script computes where to press from that
geometry (no pixel is hard-coded) and checks the events and the state.

Small, deterministic data (ROWS=200, a fixed seed): RATE=0 for exact
checks, RATE=50 for the checks under a live stream, which check
invariants rather than exact rows. The `group.*` scenarios group the
trades through the group panel, its chips, chevrons, keys and menus, under
a stream and across a restart; `footer.*`, `paint.*` and `fade.*` check
group footers, the change arrows and summary stamps, and removed trades
fading out; `tree.*` drive the portfolio tree; `dock.*` restart the dock
with a grouped tab that was never shown. `edit.*`, `widget.*`, `export.*`
and `drag.*` drive the `orders` example (ORDERS_E2E_LOG,
examples/orders/e2e.rs): editors, the server's answers, a conflict and a
fill during an edit, the checkbox and the Cancel button, the CSV export and
dragging orders to the basket and to an account.

Run inside the container by script/e2e/run.sh (an X server is required):
    python3 -I script/e2e/e2e.py [scenario-prefix ...]
Out: out/linux/e2e/ (logs, screenshots, summary.txt). Exit 1 if any
scenario failed.
"""

import json
import os
import pathlib
import subprocess
import sys
import time

OUT = pathlib.Path(os.environ.get("E2E_OUT", "out/linux/e2e"))
BIN = os.environ.get("BLOTTER_BIN", "/target/release/examples/blotter")
PORTFOLIO_BIN = os.environ.get("PORTFOLIO_BIN", "/target/release/examples/portfolio")
DOCK_BIN = os.environ.get("DOCK_BIN", "/target/release/examples/dock_layout")
ORDERS_BIN = os.environ.get("ORDERS_BIN", "/target/release/examples/orders")
ROWS = 200


class Fail(Exception):
    pass


class Skip(Exception):
    pass


def xdo(*args):
    subprocess.run(
        ["xdotool", *map(str, args)], check=True, capture_output=True, timeout=30
    )


def check(condition, message):
    if not condition:
        raise Fail(message)


class Blotter:
    """One `blotter` process and its log."""

    BIN = BIN
    LOG = "BLOTTER_E2E_LOG"
    DEFAULTS = dict(ROWS=ROWS, RATE=0, CHURN=0, RELOAD_MS=600)

    def __init__(self, name, **env):
        self.name = name
        self.log_path = OUT / f"{name}.jsonl"
        self.quit_path = pathlib.Path(f"{self.log_path}.quit")
        for path in (self.log_path, self.quit_path):
            path.unlink(missing_ok=True)
        full_env = dict(os.environ)
        full_env.update({key: str(value) for key, value in self.DEFAULTS.items()})
        full_env[self.LOG] = str(self.log_path)
        for unattended in ("BLOTTER_SECONDS", "PORTFOLIO_SECONDS", "ORDERS_SECONDS"):
            full_env.pop(unattended, None)
        full_env.update({key: str(value) for key, value in env.items()})
        self.out = open(OUT / f"{name}.out", "w")
        self.proc = subprocess.Popen(
            [self.BIN], env=full_env, stdout=self.out, stderr=subprocess.STDOUT
        )
        self.lines = []
        self._buffer = ""
        self._file = None
        self.wid = None
        self.scale = 1.0

    # The log.

    def read(self):
        if self._file is None:
            if not self.log_path.exists():
                return
            self._file = open(self.log_path)
        self._buffer += self._file.read()
        *complete, self._buffer = self._buffer.split("\n")
        self.lines.extend(json.loads(line) for line in complete if line)

    def mark(self):
        self.read()
        return len(self.lines)

    def wait(self, predicate, what, since=0, timeout=5.0):
        """The first line from `since` on that matches."""
        deadline = time.monotonic() + timeout
        ix = since
        while True:
            self.read()
            while ix < len(self.lines):
                if predicate(self.lines[ix]):
                    return self.lines[ix]
                ix += 1
            if self.proc.poll() is not None:
                raise Fail(f"blotter exited ({self.proc.returncode}) waiting for {what}")
            if time.monotonic() > deadline:
                raise Fail(f"timed out after {timeout:g} s waiting for {what}")
            time.sleep(0.02)

    def event(self, name, since, what=None, timeout=5.0, **fields):
        def matches(line):
            return (
                line["type"] == "event"
                and line["event"] == name
                and all(line.get(key) == value for key, value in fields.items())
            )

        return self.wait(matches, what or f"{name} {fields}", since, timeout)

    def state(self, predicate, what, since=0, timeout=5.0):
        return self.wait(
            lambda line: line["type"] == "state" and predicate(line), what, since, timeout
        )

    def events(self, since, name=None):
        self.read()
        return [
            line for line in self.lines[since:]
            if line["type"] == "event" and (name is None or line["event"] == name)
        ]

    def latest(self, kind):
        self.read()
        for line in reversed(self.lines):
            if line["type"] == kind:
                return line
        raise Fail(f"no {kind} line yet")

    def state_after(self, line):
        """The state line written right after an event line."""
        return self.wait(
            lambda other: other["type"] == "state" and other["seq"] > line["seq"],
            "the state after an event",
            self.lines.index(line),
        )

    # The window.

    def ready(self, state):
        """The first data set on screen with its first row selected."""
        return state["rows"] > 0 and state["selection"]["len"] == 1 and state["visible"]

    def start(self):
        """Waits until `ready`, then finds the window and gives it the
        keyboard."""
        self.state(self.ready, "the first data set on screen", timeout=30)
        geometry = self.wait(lambda line: line["type"] == "geometry", "geometry", timeout=10)
        self.scale = geometry["scale"]
        found = subprocess.run(
            ["xdotool", "search", "--sync", "--onlyvisible", "--pid", str(self.proc.pid)],
            check=True, capture_output=True, text=True, timeout=20,
        ).stdout.split()
        check(found, "no window of the blotter process")
        self.wid = found[0]
        xdo("windowfocus", "--sync", self.wid)
        # Let a frame or two settle after the focus.
        time.sleep(0.3)
        return self

    def quit(self, timeout=20):
        """Quits through `App::quit` (the quit file) and returns the exit code."""
        self.quit_path.touch()
        try:
            return self.proc.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
            raise Fail(f"blotter did not quit within {timeout} s")
        finally:
            self.out.close()

    def kill(self):
        if self.proc.poll() is None:
            self.proc.kill()
            self.proc.wait()
        self.out.close()

    def screenshot(self, name):
        subprocess.run(
            ["import", "-window", "root", str(OUT / f"{name}.png")],
            capture_output=True, timeout=30,
        )

    # Geometry, in window logical pixels.

    def geometry(self):
        return self.latest("geometry")

    def header(self, column):
        cell = self.geometry()["header"]["cells"][column]
        return cell["x"] + cell["w"] / 2, cell["y"] + cell["h"] / 2

    def header_cell(self, column):
        return self.geometry()["header"]["cells"][column]

    def column_x(self, column="instrument"):
        return self.header(column)[0]

    def row_y(self, visible_ix):
        """The middle of the `visible_ix`-th data row on screen (the body
        scrolled to a whole row)."""
        geometry = self.geometry()
        return geometry["rows_top"] + (visible_ix + 0.5) * geometry["row_height"]

    def band_y(self, edge):
        geometry = self.geometry()
        return geometry["pinned"][edge]["y"] + geometry["row_height"] / 2

    def control(self, name):
        rect = self.geometry()["controls"].get(name)
        check(rect, f"no toolbar control {name!r} in the geometry")
        return rect["x"] + rect["w"] / 2, rect["y"] + rect["h"] / 2

    def panel(self):
        panel = self.geometry().get("group_panel")
        check(panel, "no group panel in the geometry")
        return panel

    def chip(self, level, part="chip"):
        """The middle of a chip of the group panel, or of its remove button."""
        chips = self.panel()["chips"]
        check(chips, "no chips in the geometry: build blotter with test-support")
        rect = chips["levels"].get(level, {}).get(part)
        check(rect, f"no {part} of the {level} chip in the geometry")
        return center(rect)

    def caption(self, path):
        """The painted caption of the group at `path`, from the geometry."""
        found = [c for c in self.geometry()["captions"] if c["path"] == path]
        check(found, f"the caption {path} is not on screen")
        return found[0]

    def chevron(self, path):
        caption = self.caption(path)
        return caption["chevron"]["x"] + caption["chevron"]["w"] / 2, caption["y"] + caption["h"] / 2

    def caption_text(self, path):
        """A point on a caption's text: right of the chevron lane, clear of
        the aggregate cells."""
        caption = self.caption(path)
        return caption["chevron"]["x"] + caption["chevron"]["w"] + 30, caption["y"] + caption["h"] / 2

    def outside(self):
        """A point on the toolbar between its controls, off the grid."""
        controls = self.geometry()["controls"]
        reload, cancel = controls["reload"], controls["cancel-reload"]
        # Right of Cancel reload, before the View group.
        return cancel["x"] + cancel["w"] + 4, reload["y"] + reload["h"] / 2

    # Input.

    def _at(self, x, y):
        return ["mousemove", "--window", self.wid, round(x * self.scale), round(y * self.scale)]

    def move(self, x, y):
        xdo(*self._at(x, y))

    def click(self, x, y, button=1, mods=(), repeat=1):
        args = self._at(x, y)
        for mod in mods:
            args += ["keydown", mod]
        args += ["click", "--repeat", repeat, "--delay", 80, button]
        for mod in reversed(mods):
            args += ["keyup", mod]
        xdo(*args)

    def drag(self, start, end, steps=12, pause=0.03):
        x0, y0 = start
        x1, y1 = end
        xdo(*self._at(x0, y0), "mousedown", 1)
        for step in range(1, steps + 1):
            t = step / steps
            xdo(*self._at(x0 + (x1 - x0) * t, y0 + (y1 - y0) * t))
            time.sleep(pause)
        time.sleep(0.1)
        xdo("mouseup", 1)

    def key(self, *keys):
        xdo("key", "--delay", 60, *keys)

    def type(self, text):
        xdo("type", "--delay", 60, text)

    def click_row(self, visible_ix, mods=(), column="instrument"):
        self.click(self.column_x(column), self.row_y(visible_ix), mods=mods)


def center(rect):
    return rect["x"] + rect["w"] / 2, rect["y"] + rect["h"] / 2


def clipboard():
    return subprocess.run(
        ["xclip", "-o", "-selection", "clipboard"],
        check=True, capture_output=True, text=True, timeout=10,
    ).stdout


def settle(seconds=0.6):
    """Longer than the double-click interval, so two clicks stay two."""
    time.sleep(seconds)


def selection_after(app, since, what):
    """The selection after the one SelectionChange from `since` on."""
    change = app.event("SelectionChange", since, what)
    return change["keys"]


def column_index(state, column):
    """The index of `column` in a `*_values` row (the key comes first)."""
    return 1 + [c["id"] for c in state["columns"]].index(column)


# Phase A: RATE=0, exact checks.


def scenario_startup(app):
    state = app.latest("state")
    geometry = app.geometry()
    check(state["rows"] == ROWS, f"{state['rows']} rows, not {ROWS}")
    check(state["sort"] == [["last", "desc"]], f"sort {state['sort']}")
    check(state["pinned"] == {"top": [10], "bottom": [ROWS]}, f"pinned {state['pinned']}")
    cells = geometry["header"]["cells"]
    check(len(cells) == 8, f"{len(cells)} header cells")
    check(geometry["scale"] == 1, f"scale {geometry['scale']}")
    check(geometry["controls"].keys() >= {"Live", "Hold", "Manual", "refresh-order",
                                          "reload", "cancel-reload", "group-panel",
                                          "groups-menu", "arrows"},
          f"controls {sorted(geometry['controls'])}")
    # The group panel shows, empty; arrows paint the changes.
    panel = geometry["group_panel"]
    check(panel and panel["chips"] == {"levels": {}, "end": None}, f"group panel {panel}")
    check(geometry["header"]["y"] == panel["y"] + panel["h"], "the header is not under the panel")
    check(state["group_by"] == [] and state["change_paint"] == "directional",
          f"grouping {state['group_by']}, paint {state['change_paint']}")
    app.screenshot("phase-a-start")


def restore_sort(app):
    """Clicks Last until the sort is Last descending alone, as at start
    (StandardSortCycle: a plain click goes ascending, descending, none)."""
    for _ in range(3):
        if app.latest("state")["sort"] == [["last", "desc"]]:
            break
        since = app.mark()
        app.click(*app.header("last"))
        app.event("SortChange", since, "a click on Last")
        app.state(lambda s: True, "the state after the sort", since)
        settle()
    check(app.latest("state")["sort"] == [["last", "desc"]], "the sort back to Last descending")


def scenario_sort_cycle(app):
    for expected in (["bid", "asc"], ["bid", "desc"], None):
        since = app.mark()
        app.click(*app.header("bid"))
        change = app.event("SortChange", since, origin="User")
        check(change["sort"] == ([expected] if expected else []),
              f"sort {change['sort']}, expected {expected}")
        if expected:
            app.event("ProjectionChange", since)

            def sorted_on_screen(state):
                bid = column_index(state, "bid")
                values = [float(row[bid]) for row in state["top_values"]]
                return state["sort"] == [expected] and values == sorted(
                    values, reverse=expected[1] == "desc")

            # `visible` is the last painted frame: it follows the event.
            app.state(sorted_on_screen, f"rows by bid {expected[1]} on screen", since)
        settle()


def scenario_sort_shift_priority(app):
    since = app.mark()
    app.click(*app.header("bid"))
    app.event("SortChange", since, sort=[["bid", "asc"]])
    settle()
    since = app.mark()
    app.click(*app.header("ask"), mods=["shift"])
    change = app.event("SortChange", since, origin="User")
    check(change["sort"] == [["bid", "asc"], ["ask", "asc"]], f"sort {change['sort']}")
    settle()
    restore_sort(app)


def scenario_sort_clear_all(app):
    since = app.mark()
    app.click(*app.header("bid"))
    app.event("SortChange", since, sort=[["bid", "asc"]])
    settle()
    since = app.mark()
    app.click(*app.header("ask"), mods=["shift"])
    app.event("SortChange", since, sort=[["bid", "asc"], ["ask", "asc"]])
    settle()
    since = app.mark()
    header_menu(app, "bid", "clear_all_sorting")
    change = app.event("SortChange", since, "Clear all sorting")
    check(change["sort"] == [] and change["origin"] == "User", f"Clear all sorting: {change}")
    app.event("ProjectionChange", since, "the unsorted order shown")
    settle(1.0)
    changes = app.events(since, "SortChange")
    check(len(changes) == 1, f"{len(changes)} SortChange for Clear all sorting")
    state = app.latest("state")
    check(state["sort"] == [], f"sort after Clear all sorting: {state['sort']}")
    restore_sort(app)


def scenario_selection_mouse(app):
    visible = app.latest("state")["visible"]
    since = app.mark()
    app.click_row(2)
    keys = selection_after(app, since, "the click's SelectionChange")
    check(keys == [visible[2]], f"click: {keys}, expected [{visible[2]}]")
    focus = app.event("FocusChange", since)
    check(focus["key"] == visible[2] and focus["origin"] == "User", f"focus {focus}")
    settle()
    check(len(app.events(since, "SelectionChange")) == 1, "one SelectionChange per click")

    since = app.mark()
    app.click_row(5, mods=["shift"])
    keys = selection_after(app, since, "Shift-click")
    check(keys == sorted(visible[2:6]), f"Shift-click: {keys}")
    settle()

    since = app.mark()
    app.click_row(7, mods=["ctrl"])
    keys = selection_after(app, since, "Ctrl-click adds")
    check(keys == sorted(visible[2:6] + [visible[7]]), f"Ctrl-click: {keys}")
    settle()
    # Ctrl is the toggle modifier off macOS: no menu (StandardContextMenuGesture).
    check(not app.latest("state")["menu"], "Ctrl-click opened the context menu")

    since = app.mark()
    app.click_row(3, mods=["ctrl"])
    keys = selection_after(app, since, "Ctrl-click removes")
    expected = sorted([visible[2]] + visible[4:6] + [visible[7]])
    check(keys == expected, f"Ctrl-click again: {keys}, expected {expected}")
    settle()


def scenario_selection_drag(app):
    visible = app.latest("state")["visible"]
    x = app.column_x()
    since = app.mark()
    app.drag((x, app.row_y(10)), (x, app.row_y(14)), steps=8, pause=0.05)
    keys = selection_after(app, since, "the drag's SelectionChange")
    check(keys == sorted(visible[10:15]), f"drag 10..14: {keys}")
    settle()
    changes = app.events(since, "SelectionChange")
    check(len(changes) == 1, f"{len(changes)} SelectionChange for one drag")


def scenario_selection_keyboard(app):
    visible = app.latest("state")["visible"]
    app.click_row(1)
    app.state(lambda s: s["selection"]["keys"] == [visible[1]], "row 1 selected")
    settle()

    since = app.mark()
    app.key("Down")
    keys = selection_after(app, since, "Down")
    check(keys == [visible[2]], f"Down: {keys}")

    since = app.mark()
    app.key("shift+Down")
    keys = selection_after(app, since, "Shift+Down")
    check(keys == sorted(visible[2:4]), f"Shift+Down: {keys}")
    # Its FocusChange follows the SelectionChange: wait for it, or PageDown
    # below would take it for its own.
    state = app.state_after(app.event("FocusChange", since, "Shift+Down's focus"))

    def page_of(geometry):
        rows_area = geometry["body"]["h"] - (
            geometry["pinned"]["top"]["rows"] + geometry["pinned"]["bottom"]["rows"]
        ) * geometry["row_height"]
        return max(int(rows_area // geometry["row_height"]), 1)

    page = page_of(app.geometry())
    before = state["focus"]["ix"]
    since = app.mark()
    app.key("Next")
    focus = app.event("FocusChange", since, "PageDown")
    state = app.state_after(focus)
    # The page is the body's height when the key arrives: a geometry line
    # written meanwhile (the window settling its size) counts too.
    pages = {page, page_of(app.geometry())}
    expected = {min(before + each, ROWS - 1) for each in pages}
    check(state["focus"]["ix"] in expected,
          f"PageDown: focus at {state['focus']['ix']}, expected {before} + {sorted(pages)}")
    check(state["selection"]["keys"] == [focus["key"]], f"PageDown selects {state['selection']}")

    since = app.mark()
    app.key("ctrl+End")
    focus = app.event("FocusChange", since, "Ctrl+End")
    state = app.state_after(focus)
    check(focus["key"] == state["last_key"] and state["focus"]["ix"] == ROWS - 1,
          f"Ctrl+End: {state['focus']}, last key {state['last_key']}")

    since = app.mark()
    app.key("ctrl+Home")
    focus = app.event("FocusChange", since, "Ctrl+Home")
    state = app.state_after(focus)
    check(focus["key"] == state["first_key"] and state["focus"]["ix"] == 0,
          f"Ctrl+Home: {state['focus']}, first key {state['first_key']}")

    since = app.mark()
    app.key("ctrl+a")
    change = app.event("SelectionChange", since, "Ctrl+A")
    check(change["len"] == ROWS, f"Ctrl+A selects {change['len']}")

    since = app.mark()
    app.key("Escape")
    change = app.event("SelectionChange", since, "Escape")
    check(change["len"] == 0, f"Escape leaves {change['len']} selected")


def scenario_context_menu(app):
    visible = app.latest("state")["visible"]
    app.click_row(4)
    app.state(lambda s: s["selection"]["keys"] == [visible[4]], "row 4 selected")
    settle()

    # A right click on another row selects it first, then opens the menu.
    since = app.mark()
    app.click(app.column_x(), app.row_y(6), button=3)
    keys = selection_after(app, since, "the right click's SelectionChange")
    check(keys == [visible[6]], f"right click selects {keys}")
    app.state(lambda s: s["menu"], "the menu open", since)

    since = app.mark()
    app.key("Escape")
    state = app.state(lambda s: not s["menu"], "the menu closed by Escape", since)
    check(state["selection"]["keys"] == [visible[6]], f"Escape changed the selection: {state['selection']}")
    check(not app.events(since, "SelectionChange"), "Escape on the menu changed the selection")

    # Shift+F10 opens it at the focused cell; the primary command is first.
    since = app.mark()
    app.key("shift+F10")
    app.state(lambda s: s["menu"], "the menu open by Shift+F10", since)
    app.key("Down", "Return")
    confirm = app.event("Confirm", since, "Confirm from the primary command")
    check(confirm["keys"] == [visible[6]] and not confirm["secondary"], f"menu Confirm {confirm}")
    app.state(lambda s: not s["menu"], "the menu closed after the command", since)
    settle()


def scenario_double_click(app):
    visible = app.latest("state")["visible"]
    since = app.mark()
    app.click(app.column_x(), app.row_y(8), repeat=2)
    confirm = app.event("Confirm", since, "Confirm from a double click")
    check(confirm["keys"] == [visible[8]] and not confirm["secondary"], f"double click {confirm}")
    settle()


def scenario_copy(app):
    visible = app.latest("state")["visible"]
    app.click_row(0)
    app.state(lambda s: s["selection"]["keys"] == [visible[0]], "row 0 selected")
    settle()
    since = app.mark()
    app.click_row(2, mods=["shift"])
    app.event("SelectionChange", since, "Shift-click")
    state = app.state(lambda s: s["scoped"]["len"] == 3, "three rows in scope", since)
    check(state["scoped"]["keys"] == visible[:3], f"scope {state['scoped']['keys']}")
    expected = "\n".join("\t".join(row[1:]) for row in state["scoped_values"])
    app.key("ctrl+c")
    deadline = time.monotonic() + 3
    text = None
    while time.monotonic() < deadline:
        try:
            text = clipboard()
        except subprocess.CalledProcessError:
            text = None
        if text == expected:
            break
        time.sleep(0.1)
    check(text == expected, f"clipboard {text!r}, expected {expected!r}")
    lines = text.split("\n")
    check(len(lines) == 3 and all(len(line.split("\t")) == len(state["columns"]) for line in lines),
          "three lines of all visible columns")
    check([int(line.split("\t")[0]) for line in lines] == visible[:3], "keys in display order")


def scenario_quick_filter(app):
    since = app.mark()
    app.key("ctrl+f")
    time.sleep(0.3)
    app.type("INS00 Sell")

    def filtered(state):
        instrument, side = column_index(state, "instrument"), column_index(state, "side")
        return state["filter"] == "INS00 Sell" and 0 < state["rows"] < ROWS and all(
            row[instrument].startswith("INS00") and row[side] == "Sell"
            for row in state["top_values"])

    # Every keystroke rebuilds; the order of the whole text comes last.
    state = app.state(filtered, "only rows with INS00 and Sell on screen", since)
    settle(1.0)
    state = app.latest("state")
    check(filtered(state), f"the filtered rows changed: {[row[:4] for row in state['top_values']]}")
    check(len(state["top_values"]) == min(state["rows"], 20), "every filtered row on screen")
    check(state["pinned"] == {"top": [10], "bottom": [ROWS]}, "the bands stay")
    filtered_rows = state["rows"]

    since = app.mark()
    app.key("ctrl+a", "BackSpace")
    state = app.state(lambda s: s["filter"] == "" and s["rows"] == ROWS, "the filter cleared", since)
    check(filtered_rows < state["rows"], "every row back")
    # The keyboard back to the grid.
    app.click_row(0)
    settle()


# Frames that showed the reloaded rows under the progress, per reload
# (`reload.same_frame`).
RELOAD_LAGS = []


def reload_and_check(app):
    """F5: the progress shows over the old rows and does not leave before
    the reloaded rows are shown. Records how many frames showed the new
    rows with the bar still up for `reload.same_frame`. Returns the
    ProjectionChange of the reloaded rows."""
    since = app.mark()
    app.key("F5")
    on = app.state(lambda s: s["progress"], "the progress on", since)
    start = app.lines.index(on)
    off = app.state(lambda s: not s["progress"], "the progress off", start, timeout=10)
    replaced = app.event("ProjectionChange", start, "the reloaded rows", timeout=10)
    version = replaced["store_version"]
    shown = app.state(lambda s: s["projection_version"] >= version, "the reloaded rows", start)
    check(off["projection_version"] >= version,
          f"the progress left over projection {off['projection_version']}, "
          f"before the reloaded rows ({version})")
    RELOAD_LAGS.append(off["frames"] - shown["frames"])
    return replaced


def scenario_reload_same_frame(_app):
    # blotter ends a reload in one `GridWriter::batch` (finish_reload, then
    # set_progress(None)), so the reloaded rows and the cleared progress
    # share a snapshot: the bar leaves in the same frame as the new rows.
    if not RELOAD_LAGS:
        raise Skip("no reload ran in this phase")
    lagged = sum(1 for lag in RELOAD_LAGS if lag)
    check(lagged == 0,
          f"the bar outlived the reloaded rows by a frame in {lagged} of "
          f"{len(RELOAD_LAGS)} reloads")


def scenario_reload_keeps_selection(app):
    visible = app.latest("state")["visible"]
    app.click_row(3)
    app.state(lambda s: s["selection"]["keys"] == [visible[3]], "row 3 selected")
    settle()
    before = app.latest("state")["projection_version"]
    replaced = reload_and_check(app)
    check(replaced["store_version"] > before, "a newer data set")
    settle(1.0)
    state = app.latest("state")
    check(state["rows"] == ROWS, f"{state['rows']} rows after the reload")
    # The selection is by key and survives the reload.
    check(state["selection"]["keys"] == [visible[3]], f"selection after reload {state['selection']}")


def scenario_reload_selects_first_row(app):
    if app.latest("state")["selection"]["len"]:
        since = app.mark()
        app.key("Escape")
        app.state(lambda s: s["selection"]["len"] == 0, "the selection cleared", since)
    since = app.mark()
    app.key("F5")
    app.event("ProjectionChange", since, "the reloaded rows", timeout=10)
    # None of the selection survived, so the first row is selected.
    change = app.event("SelectionChange", since, "the first row selected", origin="Programmatic")
    state = app.state_after(change)
    check(change["keys"] == [state["first_key"]], f"selected {change['keys']}, first {state['first_key']}")


def screen_crop(name, rect):
    """A screenshot of `rect` (screen pixels) for a pixel comparison."""
    path = OUT / f"{name}.png"
    geometry = f"{rect['w']}x{rect['h']}+{rect['x']}+{rect['y']}"
    subprocess.run(["import", "-window", "root", "-crop", geometry, str(path)],
                   check=True, capture_output=True, timeout=30)
    return path


def pixels_differ(a, b):
    result = subprocess.run(["compare", "-metric", "AE", str(a), str(b), "null:"],
                            capture_output=True, text=True, timeout=30)
    return float(result.stderr.split()[0] or 0)


def scenario_pinned_rows(app):
    app.click_row(1)
    settle()
    state = app.latest("state")
    selected, x = state["selection"]["keys"], app.column_x()
    geometry = app.geometry()
    window = geometry["window"]
    band = {"x": round(window["x"] + geometry["grid"]["x"]), "w": round(geometry["grid"]["w"] - 20),
            "y": round(window["y"] + geometry["pinned"]["bottom"]["y"]), "h": round(geometry["row_height"])}
    total_before = screen_crop("pinned-total-before", band)

    # A press on a band only focuses the grid.
    since = app.mark()
    app.click(x, app.band_y("bottom"))
    settle(0.8)
    app.click(x, app.band_y("top"))
    settle(0.8)
    check(not app.events(since, "SelectionChange"), "a click on a band changed the selection")
    check(app.latest("state")["selection"]["keys"] == selected, "the selection moved")

    # Scroll the data rows with the wheel; the bands stay where they are.
    since = app.mark()
    app.move(x, app.row_y(5))
    xdo("click", "--repeat", 8, "--delay", 40, 5)
    state = app.state(lambda s: s["visible_rows"][0] > 0, "the body scrolled", since)
    settle(0.8)
    state = app.latest("state")
    check(state["pinned"] == {"top": [10], "bottom": [ROWS]}, f"pinned {state['pinned']}")
    total_after = screen_crop("pinned-total-after", band)
    changed = pixels_differ(total_before, total_after)
    check(changed == 0, f"the Total band changed {changed:g} pixels when the rows scrolled")
    since = app.mark()
    app.click(x, app.band_y("bottom"))
    settle(0.8)
    check(not app.events(since, "SelectionChange"), "a click on Total after scrolling selected")
    # The first data row under the top band is the first painted row.
    first = app.latest("state")["visible"][0]
    since = app.mark()
    app.click(x, app.row_y(0))
    keys = selection_after(app, since, "a click on the first row under Index")
    check(keys == [first], f"first row under the band: {keys}, expected [{first}]")
    settle()
    since = app.mark()
    app.key("ctrl+Home")
    # `visible` is the painted frame: wait for the frame at the top too.
    app.state(lambda s: s["visible_rows"][0] == 0 and s["visible"][:1] == [s["first_key"]],
              "back at the top, painted", since)


def clicks(app, since):
    return app.events(since, "Click")


def check_click(click, kind, button="Left", modifiers=(), click_count=1, **target):
    expected = {"kind": kind, **target}
    check(click["target"] == expected and click["button"] == button
          and click["modifiers"] == list(modifiers) and click["click_count"] == click_count,
          f"Click {click['target']} {click['button']} {click['modifiers']} "
          f"x{click['click_count']}, expected {expected} {button} {list(modifiers)} x{click_count}")


def scenario_click_cell(app):
    state = app.latest("state")
    selected = set(state["selection"]["keys"])
    ix = next(ix for ix in range(4, 12) if state["visible"][ix] not in selected)
    key = state["visible"][ix]
    since = app.mark()
    app.click_row(ix)
    click = app.event("Click", since, "the cell's Click")
    check_click(click, "cell", key=key, column="instrument")
    selection = app.event("SelectionChange", since, "the click's SelectionChange")
    focus = app.event("FocusChange", since, "the click's FocusChange")
    check(selection["seq"] < focus["seq"] < click["seq"],
          f"order: SelectionChange #{selection['seq']}, FocusChange #{focus['seq']}, "
          f"Click #{click['seq']}")
    settle()
    check(len(clicks(app, since)) == 1, f"{len(clicks(app, since))} Clicks for one click")


def scenario_click_total(app):
    selected = app.latest("state")["selection"]["keys"]
    since = app.mark()
    app.click(app.column_x("volume"), app.band_y("bottom"))
    click = app.event("Click", since, "the Total row's Click")
    check_click(click, "pinned", edge="bottom", key=ROWS, column="volume")
    settle()
    check(not app.events(since, "SelectionChange"), "a click on Total changed the selection")
    check(app.latest("state")["selection"]["keys"] == selected, "the selection moved")


def scenario_click_header(app):
    since = app.mark()
    app.click(*app.header("volume"), mods=["shift"])
    click = app.event("Click", since, "the header's Click")
    check_click(click, "header", modifiers=["shift"], column="volume")
    change = app.event("SortChange", since, "the header click's SortChange")
    check(change["seq"] < click["seq"], f"SortChange #{change['seq']} after Click #{click['seq']}")
    check(change["sort"] == [["last", "desc"], ["volume", "asc"]], f"sort {change['sort']}")
    settle()
    restore_sort(app)


def scenario_click_empty(app):
    # The header right of the columns: between the scrolling zone's last
    # column and the right zone.
    cells = app.geometry()["header"]["cells"].values()
    end = max(c["x"] + c["w"] for c in cells if c["zone"] != "right")
    right = min(c["x"] for c in cells if c["zone"] == "right")
    check(right - end > 40, f"no empty header between {end} and {right}")
    since = app.mark()
    app.click((end + right) / 2, app.geometry()["header"]["y"] + app.geometry()["header"]["h"] / 2)
    click = app.event("Click", since, "the empty area's Click")
    check_click(click, "empty")
    settle()
    for name in ("SortChange", "SelectionChange", "LayoutChange"):
        check(not app.events(since, name), f"a click on the empty header gave {name}")


def scenario_click_double(app):
    state = app.latest("state")
    key = state["visible"][8]
    since = app.mark()
    app.click(app.column_x(), app.row_y(8), repeat=2)
    second = app.event("Click", since, "the second Click", click_count=2)
    confirm = app.event("Confirm", since, "the double click's Confirm")
    first = app.event("Click", since, "the first Click", click_count=1)
    check_click(first, "cell", key=key, column="instrument")
    check_click(second, "cell", click_count=2, key=key, column="instrument")
    check(confirm["keys"] == [key], f"Confirm {confirm['keys']}")
    check(first["seq"] < confirm["seq"] < second["seq"],
          f"order: Click x1 #{first['seq']}, Confirm #{confirm['seq']}, Click x2 #{second['seq']}")
    settle()


def scenario_click_drag(app):
    visible = app.latest("state")["visible"]
    x = app.column_x()
    since = app.mark()
    app.drag((x, app.row_y(10)), (x, app.row_y(13)), steps=8, pause=0.05)
    keys = selection_after(app, since, "the drag's SelectionChange")
    check(keys == sorted(visible[10:14]), f"drag 10..13: {keys}")
    settle(1.0)
    check(not clicks(app, since), f"a drag gave {clicks(app, since)}")


# Phase B: a live stream (RATE=50), invariants.


def scenario_hold_pointer(app):
    app.move(*app.outside())
    settle(1.0)
    since = app.mark()
    app.move(app.column_x("volume"), app.row_y(12))
    held = app.event("OrderHeldChange", since, "the order held with the pointer over the body",
                     timeout=5, held=True)
    shown = app.state_after(held)["visible"]
    time.sleep(0.5)
    check(app.latest("state")["visible"] == shown, "rows moved while the order was held")
    # A click under the stream, on a held order, selects the row under it.
    since = app.mark()
    app.click_row(3)
    keys = selection_after(app, since, "a click under the stream")
    check(keys == [shown[3]], f"click under the stream: {keys}, expected [{shown[3]}]")
    since = app.mark()
    app.move(*app.outside())
    app.event("OrderHeldChange", since, "the held order applied when the pointer left",
              timeout=3, held=False)
    app.event("ProjectionChange", since, "the new order shown")
    settle(1.0)
    state = app.latest("state")
    check(state["selection"]["keys"] == [shown[3]] and state["focus"]["key"] == shown[3],
          f"the selection or focus moved with the reorder: {state['selection']}, {state['focus']}")


def scenario_hold_expires(app):
    app.move(*app.outside())
    settle(1.0)
    since = app.mark()
    app.move(app.column_x("volume"), app.row_y(12))
    held = app.event("OrderHeldChange", since, "the order held", timeout=5, held=True)
    # HoldReorder::default(): at most 3 s, then the order shows under the pointer.
    released = app.event("OrderHeldChange", app.lines.index(held), "the hold expired",
                         timeout=5, held=False)
    elapsed = (released["t"] - held["t"]) / 1000
    check(2.5 <= elapsed <= 4.5, f"held for {elapsed:.1f} s, expected about 3 s")
    app.move(*app.outside())
    settle()


def scenario_manual_refresh(app):
    since = app.mark()
    app.click(*app.control("Manual"))
    app.move(*app.outside())
    app.event("OrderHeldChange", since, "an order held under Manual", timeout=5, held=True)
    app.state(lambda s: s["held"], "is_order_held", since)
    settle(0.3)
    since = app.mark()
    app.click(*app.control("refresh-order"))
    app.event("OrderHeldChange", since, "Refresh order", held=False)
    shown = app.event("ProjectionChange", since, "the refreshed order shown")
    # A user action applies at once under Manual: a header click while held.
    # The stream holds the next order soon, maybe before this line runs.
    app.event("OrderHeldChange", app.lines.index(shown), "held again", timeout=5, held=True)
    since = app.mark()
    app.click(*app.header("volume"))
    released = app.event("OrderHeldChange", since, "the held order dropped", held=False)
    sorted_ = app.event("SortChange", since, "the sort", sort=[["volume", "asc"]])
    shown = app.event("ProjectionChange", since, "the sorted order shown")
    check(released["seq"] < sorted_["seq"] < shown["seq"],
          f"order of events: held false #{released['seq']}, sort #{sorted_['seq']}, shown #{shown['seq']}")
    settle()
    since = app.mark()
    app.click(*app.control("Live"))
    app.state(lambda s: not s["held"], "Live applies the held order", since)
    settle()


def scenario_reload_live(app):
    # Under Manual the stream's orders are held, so the one order shown is
    # the reloaded data set, which shows under any policy.
    app.click(*app.control("Manual"))
    settle(1.0)
    since = app.mark()
    app.click_row(2)
    selected = selection_after(app, since, "a row selected before the reload")
    app.move(*app.outside())
    settle(1.0)
    since = app.mark()
    reload_and_check(app)
    check(len(app.events(since, "ProjectionChange")) == 1, "one order shown under Manual")
    settle(1.0)
    state = app.latest("state")
    check(state["rows"] == ROWS, f"{state['rows']} rows after the reload")
    check(state["selection"]["keys"] == selected, f"selection {state['selection']} after reload")
    app.click(*app.control("Live"))
    settle()


# Phase C: the layout across a restart (BLOTTER_LAYOUT), RATE=0.


def header_menu_downs(state, column, item):
    """Down presses that select `item` in the header menu of `column`, from
    a model of GridMenuContext::standard_items. The first Down selects the
    first item; each later one the next enabled item (separators and
    disabled items are skipped)."""
    zone = {c["id"]: c["zone"] for c in state["columns"]}[column]
    levels = [level["column"] for level in state.get("group_by", [])]
    items = [
        ("sort_ascending", True),
        ("sort_descending", True),
        ("clear_sort", column in dict(state["sort"])),
        ("clear_all_sorting", bool(state["sort"])),
        ("autofit_column", True),
        ("autofit_columns", True),
        ("hide_column", len(state["columns"]) > 1),
        ("show_all_columns", bool(state["hidden"])),
        # The column chooser.
        ("choose_columns", True),
        *[(f"freeze_{edge}", True) for edge in ("left", "right") if zone != edge],
        *([("unfreeze", True)] if zone != "scrolling" else []),
        ("reset_columns", True),
        # The grouping items end the column items.
        ("group_by", column not in levels),
        ("ungroup", column in levels),
        ("clear_grouping", bool(levels)),
        ("copy", state["scoped"]["len"] > 0),
        ("copy_all", True),
    ]
    ix = [name for name, _ in items].index(item)
    check(items[ix][1], f"{item} is disabled in the {column} header menu")
    return 1 + sum(1 for _, enabled in items[1:ix + 1] if enabled)


def header_menu(app, column, item):
    downs = header_menu_downs(app.latest("state"), column, item)
    since = app.mark()
    app.click(*app.header(column), button=3)
    app.state(lambda s: s["menu"], f"the {column} header menu open", since)
    app.key(*(["Down"] * downs), "Return")
    app.state(lambda s: not s["menu"], "the menu closed", since)


def scenario_columns(app, saved):
    # Resize: drag the right edge of Bid's header cell.
    cell = app.header_cell("bid")
    edge, y = cell["x"] + cell["w"] - 1, cell["y"] + cell["h"] / 2
    since = app.mark()
    app.drag((edge, y), (edge + 60, y), steps=10)
    change = app.event("LayoutChange", since, "the resize", origin="User")
    check(change["parts"] == ["Width"] and change["columns"] == ["bid"], f"resize: {change}")
    width = {c["id"]: c["width"] for c in app.state_after(change)["columns"]}["bid"]
    check(abs(width - (cell["w"] + 60)) <= 2, f"Bid {width} px, expected {cell['w'] + 60}")
    settle()

    # Move: drag Volume's header onto the left half of Bid's.
    since = app.mark()
    bid = app.header_cell("bid")
    app.drag(app.header("volume"), (bid["x"] + 8, bid["y"] + bid["h"] / 2), steps=16, pause=0.05)
    change = app.event("LayoutChange", since, "the move", origin="User")
    check("Order" in change["parts"], f"move: {change}")
    order = [c["id"] for c in app.state_after(change)["columns"]]
    check(order.index("volume") == order.index("bid") - 1, f"order after the move: {order}")
    settle()

    # Hide and freeze through the header menu.
    since = app.mark()
    header_menu(app, "side", "hide_column")
    change = app.event("LayoutChange", since, "Hide column")
    check(change["parts"] == ["Visibility"] and change["columns"] == ["side"], f"hide: {change}")
    check("side" in app.state_after(change)["hidden"], "Side hidden")
    settle()
    since = app.mark()
    header_menu(app, "ask", "freeze_left")
    change = app.event("LayoutChange", since, "Freeze left")
    check("Zone" in change["parts"] and "ask" in change["columns"], f"freeze: {change}")
    zones = {c["id"]: c["zone"] for c in app.state_after(change)["columns"]}
    check(zones["ask"] == "left", f"Ask in {zones['ask']}")
    settle()

    # Sort.
    since = app.mark()
    app.click(*app.header("volume"))
    app.event("SortChange", since, sort=[["volume", "asc"]])
    settle(1.0)
    saved.update(app.latest("state"))


def scenario_layout_restored(app, saved):
    state = app.latest("state")
    for field in ("columns", "hidden", "sort"):
        check(state[field] == saved[field],
              f"{field} after the restart: {state[field]}, before: {saved[field]}")

# Groups. Phase "groups": RATE=0, BLOTTER_TOTALS=1, the group panel
# shown, no grouping at start. Each scenario leaves the grouping the
# next one starts from: [] → [instrument, side] → [side] → … → [side].


def level(column, direction="asc"):
    return {"column": column, "direction": direction}


def captions_of(state, depth=0):
    """The captions among the state's first lines, at `depth`."""
    return [line for line in state["lines"]
            if line["kind"] == "caption" and len(line["path"]) == depth + 1]


def store_rows(app):
    """The values of every trade, by column id (the `rows` line, RATE=0)."""
    rows = app.latest("rows")
    columns = rows["columns"]
    return [dict(zip(["key", *columns], row)) for row in rows["rows"]]


def tsv_row(state, values):
    """A trade as Copy writes it: the visible columns in order."""
    return "\t".join(str(values[c["id"]]) for c in state["columns"])


def wait_clipboard(predicate, what, timeout=3.0):
    deadline = time.monotonic() + timeout
    text = None
    while time.monotonic() < deadline:
        try:
            text = clipboard()
        except subprocess.CalledProcessError:
            text = None
        if text is not None and predicate(text):
            return text
        time.sleep(0.1)
    raise Fail(f"clipboard: {what}; it holds {text!r:.300}")


def to_top(app):
    """Ctrl+Home unless the first line is painted already: a regrouping
    keeps the focused trade in view, so the first captions may be off
    screen. Waits for the frame that paints the first caption."""
    def painted():
        return (app.latest("state")["visible_rows"][0] == 0
                and any(c["ix"] == 0 for c in app.geometry()["captions"]))

    if painted():
        return
    since = app.mark()
    app.key("ctrl+Home")
    app.state(lambda s: s["visible_rows"][0] == 0, "scrolled to the top", since)
    deadline = time.monotonic() + 5
    while not painted():
        if time.monotonic() > deadline:
            raise Fail("the first caption is not painted after Ctrl+Home")
        time.sleep(0.05)
    settle(0.3)


def drop_on_panel(app, column, x):
    """Drags `column`'s header into the group panel at window `x`."""
    panel = app.panel()
    app.drag(app.header(column), (x, panel["y"] + panel["h"] / 2), steps=16, pause=0.05)


def scenario_group_panel_drag(app):
    state = app.latest("state")
    check(state["group_by"] == [], f"grouped at start: {state['group_by']}")
    columns = [c["id"] for c in state["columns"]]
    panel = app.panel()
    since = app.mark()
    # An empty panel: anywhere in it.
    drop_on_panel(app, "instrument", panel["x"] + panel["w"] / 2)
    app.event("GroupChange", since, "Instrument dropped on the group panel", origin="User")
    app.state(lambda s: s["group_by"] == [level("instrument")], "grouped by Instrument", since)
    settle()
    # Right of the Instrument chip: after it.
    chips = app.panel()["chips"]
    check(chips and chips["end"], "no Instrument chip in the geometry")
    drop_on_panel(app, "side", min(chips["end"] + 60, panel["x"] + panel["w"] - 20))
    state = app.state(lambda s: s["group_by"] == [level("instrument"), level("side")]
                      and s["lines"][1]["kind"] == "caption",
                      "grouped by Instrument, then Side, laid out", since)
    changes = app.events(since, "GroupChange")
    check([c["origin"] for c in changes] == ["User", "User"], f"GroupChange: {changes}")
    # Captions on screen; the top-level groups hold every trade.
    check(state["lines"][0]["kind"] == "caption" and state["lines"][1]["kind"] == "caption",
          f"first lines {state['lines'][:2]}")
    check(state["groups"]["top_rows"] == ROWS,
          f"the top-level groups hold {state['groups']['top_rows']} trades, not {ROWS}")
    check(app.geometry()["captions"], "no caption in the geometry")
    settle()
    # The columns stayed: a drop on the panel groups, it does not move.
    check(not app.events(since, "LayoutChange"), "a drop on the panel moved a column")
    check([c["id"] for c in app.latest("state")["columns"]] == columns, "the columns moved")


def scenario_group_chips(app):
    before = [c["path"] for c in captions_of(app.latest("state"))]
    # A click on the chip reverses its groups.
    since = app.mark()
    app.click(*app.chip("instrument"))
    app.event("GroupChange", since, "a click on the Instrument chip", origin="User")
    app.event("ProjectionChange", since, "the reversed groups shown")

    def reversed_on_screen(state):
        top = [c["path"] for c in captions_of(state)]
        return (state["group_by"] == [level("instrument", "desc"), level("side")]
                and top and top == sorted(top, reverse=True) and top[0] > before[0])

    app.state(reversed_on_screen, "the instruments descending on screen", since)
    settle()
    # Its remove button takes the level out.
    since = app.mark()
    app.click(*app.chip("instrument", "remove"))
    app.event("GroupChange", since, "the Instrument chip removed", origin="User")
    app.state(lambda s: s["group_by"] == [level("side")]
              and [c["path"] for c in captions_of(s)][:1] == [["Buy"]],
              "grouped by Side alone", since)
    settle()


def scenario_group_chevron(app):
    to_top(app)
    state = app.latest("state")
    buy = state["lines"][0]
    check(buy["kind"] == "caption" and buy["path"] == ["Buy"] and buy["expanded"],
          f"first line {buy}")
    for expanded in (False, True):
        since = app.mark()
        app.click(*app.chevron(["Buy"]))
        change = app.event("ExpansionChange", since, "a press on the chevron", origin="User")
        check(change["target"] == {"group": ["Buy"]}, f"target {change['target']}")
        # Collapsed: the next line is Sell's caption; expanded: a trade.
        app.state(lambda s: s["lines"][0]["expanded"] == expanded
                  and s["lines"][1]["kind"] == ("data" if expanded else "caption"),
                  f"Buy {'expanded' if expanded else 'collapsed'} on screen", since)
        settle()
        check(not app.events(since, "Click"), "a press on a chevron is a Click")
    # A double click on the caption's text toggles it too.
    since = app.mark()
    app.click(*app.caption_text(["Buy"]), repeat=2)
    app.event("ExpansionChange", since, "a double click on the caption", origin="User")
    app.state(lambda s: not s["lines"][0]["expanded"], "Buy collapsed by a double click", since)
    settle()
    check(len(app.events(since, "ExpansionChange")) == 1, "one ExpansionChange per double click")
    since = app.mark()
    app.click(*app.chevron(["Buy"]))
    app.state(lambda s: s["lines"][0]["expanded"], "Buy expanded again", since)
    settle()


def focus_caption(app, path):
    """A press on a caption's text; waits until the caption has the focus
    (it may have had it already: no FocusChange then)."""
    since = app.mark()
    app.click(*app.caption_text(path))
    app.event("Click", since, f"the press on {path}")
    app.state(lambda s: (s["focus"] or {}).get("group") == path, f"{path} focused")


def focus_after(app, since, what):
    focus = app.event("FocusChange", since, what)
    return app.state_after(focus)["focus"]


def scenario_group_keyboard(app):
    # Two levels: Instrument dropped after Side.
    since = app.mark()
    chips = app.panel()["chips"]
    panel = app.panel()
    drop_on_panel(app, "instrument", min(chips["end"] + 60, panel["x"] + panel["w"] - 20))
    state = app.state(lambda s: s["group_by"] == [level("side"), level("instrument")]
                      and s["lines"][1]["kind"] == "caption", "grouped by Side, then Instrument",
                      since)
    settle()
    to_top(app)
    sub = state["lines"][1]
    path = sub["path"]
    check(path[0] == "Buy" and sub["expanded"], f"second line {sub}")
    # A press on the caption focuses it.
    since = app.mark()
    app.click(*app.caption_text(path))
    check(focus_after(app, since, "the caption focused")["group"] == path, "focus on the caption")
    settle()
    # `-` collapses, `+` expands.
    for key, expanded in (("minus", False), ("plus", True)):
        since = app.mark()
        app.key(key)
        change = app.event("ExpansionChange", since, key, origin="User")
        check(change["target"] == {"group": path}, f"{key}: {change['target']}")
        app.state(lambda s: s["lines"][1]["expanded"] == expanded, f"{key} on screen", since)
    # ← collapses; ← on a collapsed caption goes to its parent.
    since = app.mark()
    app.key("Left")
    app.state(lambda s: not s["lines"][1]["expanded"], "Left collapsed it", since)
    since = app.mark()
    app.key("Left")
    check(focus_after(app, since, "Left to the parent")["group"] == ["Buy"], "focus on Buy")
    # → on an expanded caption goes to its first line; on a collapsed one
    # expands it; then to the group's first trade.
    since = app.mark()
    app.key("Right")
    check(focus_after(app, since, "Right to the first line")["group"] == path,
          "focus on the first subgroup")
    since = app.mark()
    app.key("Right")
    app.state(lambda s: s["lines"][1]["expanded"], "Right expanded it", since)
    since = app.mark()
    app.key("Right")
    focus = focus_after(app, since, "Right to the first trade")
    state = app.latest("state")
    check(focus["key"] == state["lines"][2]["key"] and focus["group"] is None,
          f"focus {focus}, third line {state['lines'][2]}")
    # ↓: the next line.
    since = app.mark()
    app.key("Down")
    focus = focus_after(app, since, "Down")
    line = app.latest("state")["lines"][3]
    if line["kind"] == "data":
        check(focus["key"] == line["key"], f"Down: focus {focus}, line {line}")
    else:
        check(focus["group"] == line["path"], f"Down: focus {focus}, line {line}")
    settle()
    # Enter on a caption: the primary command over its trades.
    since = app.mark()
    app.click(*app.caption_text(["Buy"]))
    focus_after(app, since, "Buy focused")
    settle()
    count = app.latest("state")["lines"][0]["rows"]
    since = app.mark()
    app.key("Return")
    confirm = app.event("Confirm", since, "Enter on the caption")
    buy = {row["key"] for row in store_rows(app) if row["side"] == "Buy"}
    check(len(confirm["keys"]) == count and set(confirm["keys"]) == buy,
          f"Confirm of {len(confirm['keys'])} trades, the caption counts {count}, Buy has {len(buy)}")
    # Back to Side alone.
    since = app.mark()
    app.click(*app.chip("instrument", "remove"))
    app.state(lambda s: s["group_by"] == [level("side")], "Instrument ungrouped", since)
    settle()


def scenario_group_focus_not_selection(app):
    to_top(app)
    # Buy collapsed; a Sell trade selected.
    since = app.mark()
    app.click(*app.chevron(["Buy"]))
    state = app.state(lambda s: s["lines"][1]["kind"] == "caption", "Buy collapsed", since)
    settle()
    sell = state["lines"][1]
    check(sell["path"] == ["Sell"], f"second line {sell}")
    # Lines 0 and 1 are the captions: line 2 is Sell's first trade.
    since = app.mark()
    app.click_row(2)
    keys = selection_after(app, since, "a Sell trade selected")
    settle()
    # A press on Buy's caption: the focus moves there, the selection stays.
    since = app.mark()
    app.click(*app.caption_text(["Buy"]))
    check(focus_after(app, since, "Buy's caption focused")["group"] == ["Buy"], "focus on Buy")
    settle()
    check(not app.events(since, "SelectionChange"), "a press on a caption changed the selection")
    check(app.latest("state")["selection"]["keys"] == keys, "the selection moved")
    # Cmd/Ctrl+A: the trades on screen, none of the collapsed Buy.
    since = app.mark()
    app.key("ctrl+a")
    change = app.event("SelectionChange", since, "Ctrl+A")
    check(change["len"] == sell["rows"], f"Ctrl+A selected {change['len']}, Sell has {sell['rows']}")
    buy = {row["key"] for row in store_rows(app) if row["side"] == "Buy"}
    check(not buy & set(change["keys"]), "Ctrl+A selected trades of the collapsed group")
    since = app.mark()
    app.key("Escape")
    app.event("SelectionChange", since, "Escape")
    since = app.mark()
    app.click(*app.chevron(["Buy"]))
    app.state(lambda s: s["lines"][0]["expanded"], "Buy expanded again", since)
    settle()


def caption_menu_downs(item):
    """Down presses that select `item` in the menu of an expanded group
    caption: the primary command (Open N trades), the blotter's Cancel N
    trades, then GridMenuContext::group_items, Copy and Copy all."""
    items = ["open", "cancel", "collapse", "expand_all", "collapse_all", "captions_only",
             "copy", "copy_all"]
    return 1 + items.index(item)


# The toolbar's Groups menu, in order.
GROUPS_MENU = ["expand_all", "collapse_all", "captions_only", "copy_expanded"]


def groups_menu(app, item):
    """Picks `item` of the toolbar's Groups menu with the keyboard."""
    app.click(*app.control("groups-menu"))
    time.sleep(0.4)
    app.key(*(["Down"] * (1 + GROUPS_MENU.index(item))), "Return")


def scenario_group_menu(app):
    to_top(app)
    since = app.mark()
    app.click(*app.caption_text(["Buy"]), button=3)
    app.state(lambda s: s["menu"], "the caption's menu open", since)
    # The items of a caption's menu: Captions only is the sixth.
    since = app.mark()
    app.key(*(["Down"] * caption_menu_downs("captions_only")), "Return")
    change = app.event("ExpansionChange", since, "Captions only", origin="User")
    check(change["target"] is None, f"target {change['target']}")
    state = app.state(lambda s: not s["menu"] and s["expand_depth"] == "captions"
                      and all(line["kind"] == "caption" for line in s["lines"]),
                      "captions only on screen", since)
    check([line["path"] for line in state["lines"]] == [["Buy"], ["Sell"]],
          f"lines {state['lines']}")
    settle()
    # The toolbar's Groups menu: Expand all.
    since = app.mark()
    groups_menu(app, "expand_all")
    change = app.event("ExpansionChange", since, "Expand all", origin="User")
    check(change["target"] is None, f"target {change['target']}")
    app.state(lambda s: s["expand_depth"] == "all" and s["lines"][1]["kind"] == "data",
              "everything expanded", since)
    settle()


def scenario_group_totals(app):
    state = app.latest("state")
    totals = app.geometry()["totals"]
    check(totals and totals["edge"] == "bottom", f"totals {totals}")
    since = app.mark()
    app.click(app.column_x("volume"), totals["y"] + app.geometry()["row_height"] / 2)
    click = app.event("Click", since, "a click on the totals row")
    check_click(click, "totals", edge="bottom", column="volume")
    settle()
    check(not app.events(since, "SelectionChange"), "a click on the totals selected")
    rows = store_rows(app)
    values = app.latest("state")["totals"]
    check(values and values["rows"] == ROWS, f"totals {values}")
    volume = sum(int(row["volume"]) for row in rows)
    bid = min(float(row["bid"]) for row in rows)
    check(int(values["values"]["volume"]) == volume,
          f"Sum(Volume) {values['values']['volume']}, the trades {volume}")
    check(float(values["values"]["bid"]) == bid, f"Min(Bid) {values['values']['bid']}, the trades {bid}")
    check(state["sort"] == app.latest("state")["sort"], "the sort changed")


def scenario_group_copy(app):
    state = app.latest("state")
    rows = store_rows(app)
    buy = [row for row in rows if row["side"] == "Buy"]
    to_top(app)
    # Cmd/Ctrl+C on Buy's caption: its trades, no summary line.
    focus_caption(app, ["Buy"])
    settle()
    app.key("ctrl+c")
    expected = sorted(tsv_row(state, row) for row in buy)
    text = wait_clipboard(lambda t: sorted(t.split("\n")) == expected,
                          f"the {len(buy)} Buy trades")
    check(len(text.split("\n")) == len(buy), "one line per trade")
    # Copy all expanded (Groups menu): Buy's caption with its key under
    # Side and its aggregates under theirs; the totals last.
    groups_menu(app, "copy_expanded")
    text = wait_clipboard(lambda t: t.count("\n") > len(rows), "Copy all expanded")
    lines = [line.split("\t") for line in text.split("\n")]
    ix = {c["id"]: i for i, c in enumerate(state["columns"])}
    captions = [cells for cells in lines if cells[ix["side"]] in ("Buy", "Sell")
                and not cells[ix["id"]]]
    check(len(captions) == 2, f"{len(captions)} caption lines")
    buy_caption = next(cells for cells in captions if cells[ix["side"]] == "Buy")
    check(int(buy_caption[ix["volume"]]) == sum(int(row["volume"]) for row in buy),
          f"Buy's Sum(Volume) {buy_caption[ix['volume']]}")
    check(float(buy_caption[ix["bid"]]) == min(float(row["bid"]) for row in buy),
          f"Buy's Min(Bid) {buy_caption[ix['bid']]}")
    totals = lines[-1]
    check(totals[ix["id"]] == "Total" and int(totals[ix["volume"]]) == sum(
        int(row["volume"]) for row in rows), f"last line {totals}")
    trades = [cells for cells in lines[1:] if cells[ix["id"]].isdigit()]
    # Every trade and Index (its key is 10).
    check(len(trades) == len(rows) + 1, f"{len(trades)} trade lines")


# Phase "groups_live": RATE=50, CHURN=20, BLOTTER_GROUP=side.


def scenario_group_live(app):
    state = app.latest("state")
    check(state["group_by"] == [level("side")], f"grouping {state['group_by']}")
    # The pointer off the rows: the order follows the stream.
    app.move(*app.outside())
    to_top(app)
    focus_caption(app, ["Buy"])
    app.move(*app.outside())
    since = app.mark()
    app.key("minus")
    app.event("ExpansionChange", since, "Buy collapsed", origin="User")
    start = app.state(lambda s: not s["lines"][0]["expanded"], "Buy collapsed", since)
    time.sleep(3)
    app.read()
    states = [line for line in app.lines[app.lines.index(start):] if line["type"] == "state"]
    counts, volumes = set(), set()
    for state in states:
        buy = next((line for line in state["lines"] if line.get("path") == ["Buy"]), None)
        check(buy is not None, "Buy's caption left the lines")
        check(not buy["expanded"], "new trades expanded the collapsed group")
        check(state["focus"]["group"] == ["Buy"], f"the focus moved: {state['focus']}")
        counts.add(buy["rows"])
        if state.get("totals"):
            volumes.add(state["totals"]["values"]["volume"])
    check(len(states) > 5, f"{len(states)} states in 3 s")
    check(len(counts) > 1, f"Buy's count stayed {counts}")
    check(len(volumes) > 1, f"Sum(Volume) stayed {volumes}")
    since = app.mark()
    app.key("plus")
    app.state(lambda s: s["lines"][0]["expanded"], "Buy expanded again", since)


# Phases "groups_layout1" and "groups_layout2": the grouping and a collapsed
# group across a restart (BLOTTER_LAYOUT), RATE=0.


def scenario_group_layout_saved(app, saved, path):
    state = app.latest("state")
    check(state["group_by"] == [level("instrument"), level("side")],
          f"BLOTTER_GROUP: {state['group_by']}")
    first = state["lines"][0]
    since = app.mark()
    app.click(*app.chevron(first["path"]))
    app.event("ExpansionChange", since, "a group collapsed", origin="User")
    state = app.state(lambda s: not s["lines"][0]["expanded"], "collapsed on screen", since)
    # Written half a second after the change.
    deadline = time.monotonic() + 3
    while time.monotonic() < deadline:
        text = path.read_text() if path.exists() else ""
        if "group_by" in text and first["path"][0] in text:
            break
        time.sleep(0.1)
    else:
        raise Fail(f"the layout file has no grouping and toggle: {text[:300]!r}")
    saved.update(group_by=state["group_by"], expand_depth=state["expand_depth"],
                 collapsed=first["path"])


def scenario_group_layout_restored(app, saved):
    state = app.latest("state")
    check(state["group_by"] == saved["group_by"],
          f"grouping after the restart: {state['group_by']}, before: {saved['group_by']}")
    check(state["expand_depth"] == saved["expand_depth"], f"depth {state['expand_depth']}")
    first = state["lines"][0]
    check(first["path"] == saved["collapsed"] and not first["expanded"],
          f"first line {first}, collapsed before: {saved['collapsed']}")
    check(state["lines"][1]["kind"] == "caption" and len(state["lines"][1]["path"]) == 1,
          f"the collapsed group's lines are shown: {state['lines'][1]}")


# What the change paint shows (`paint` lines: the changes the blotter's
# style hooks saw painted, after the grid's change paint).


def paints(app, since):
    app.read()
    return [line for line in app.lines[since:] if line["type"] == "paint"]


def painted_trends(paint_lines, columns):
    """`(key, column) -> {trends}` of the updated data cells painted."""
    trends = {}
    for paint in paint_lines:
        for key, column, change, trend in paint["cells"]:
            if change == "updated" and column in columns:
                trends.setdefault((key, column), set()).add(trend)
    return trends


PRICES = ("bid", "ask", "last")


def cancel_trade(app, line_ix):
    """Cancel trade from the context menu of display line `line_ix` (Open
    trade, Cancel trade): the blotter removes it from the store. Returns
    its key and the mark before the command."""
    since = app.mark()
    app.click(app.column_x(), app.row_y(line_ix - app.latest("state")["visible_rows"][0]),
              button=3)
    keys = selection_after(app, since, "the right click")
    check(len(keys) == 1, f"the right click selected {keys}")
    app.state(lambda s: s["menu"], "the menu open", since)
    since = app.mark()
    app.key("Down", "Down", "Return")
    change = app.event("SelectionChange", since, "the selection lets go of the trade",
                       origin="DataRemoved")
    check(change["keys"] == [], f"selection {change['keys']}")
    return keys[0], since


def scenario_paint_falls(app):
    # The Index row sums the Volume of INS000–INS009 (model::write_summaries).
    # Cancelling one of their trades lowers it: the pinned cell is painted
    # falling. With RATE=0 nothing else changes a value.
    since = app.mark()
    app.key("ctrl+f")
    time.sleep(0.3)
    app.type("INS00")
    state = app.state(lambda s: s["filter"] == "INS00" and 0 < s["rows"] < ROWS and s["visible"],
                      "the trades of the index", since)
    settle(1.0)
    state = app.latest("state")
    instrument = column_index(state, "instrument")
    check(all(row[instrument].startswith("INS00") for row in state["top_values"]),
          f"filtered {[row[instrument] for row in state['top_values']]}")
    instruments = {row["key"]: row["instrument"] for row in store_rows(app)}
    key, cancelled = cancel_trade(app, state["visible_rows"][0])
    check(instruments[key].startswith("INS00"), f"cancelled {key} of {instruments[key]}")
    before = [entry for line in app.lines[:cancelled] if line["type"] == "paint"
              for entry in line["pinned"] if entry[1:3] == [10, "volume"]]
    check(not before, f"the Index row's Volume painted before the cancel: {before}")
    paint = app.wait(lambda line: line["type"] == "paint" and any(
        entry[:3] == ["top", 10, "volume"] for entry in line["pinned"]),
        "the Index row's Volume painted after the cancel", cancelled, timeout=5)
    volume = [entry for entry in paint["pinned"] if entry[:3] == ["top", 10, "volume"]]
    check(volume == [["top", 10, "volume", "updated", "down"]],
          f"the Index row's Volume after cancelling trade {key}: {volume}")
    check(paint["icons"] > 0, "no arrow counted in GridStats::icons")
    since = app.mark()
    app.key("ctrl+f")
    time.sleep(0.3)
    app.key("ctrl+a", "BackSpace")
    app.state(lambda s: s["filter"] == "", "the filter cleared", since)
    # The keyboard back to the grid, at the top: the focused trade kept
    # its screen line when the filter went, so the rows scrolled.
    app.click_row(0)
    settle()
    since = app.mark()
    app.key("ctrl+Home")
    app.state(lambda s: s["visible_rows"][0] == 0 and s["visible"][:1] == [s["first_key"]],
              "back at the top, painted", since)
    settle()


def scenario_paint_stream_rises(app):
    # Quotes move up and down; a trade's Volume only grows (model.rs:
    # `volume += 0..100`), so every Volume arrow points up.
    app.move(*app.outside())
    since = app.mark()
    deadline = time.monotonic() + 8
    while True:
        lines = paints(app, since)
        trends = painted_trends(lines, ("volume", *PRICES))
        prices = set().union(*(seen for (_, column), seen in trends.items()
                               if column in PRICES)) if trends else set()
        volume = any(column == "volume" for _, column in trends)
        if {"up", "down"} <= prices and volume:
            break
        if time.monotonic() > deadline:
            raise Fail(f"under the stream: price arrows {prices}, a Volume arrow: {volume}")
        time.sleep(0.1)
    for (key, column), seen in trends.items():
        check(None not in seen, f"trade {key} {column}: an update painted without an arrow")
        if column == "volume":
            check(seen == {"up"}, f"trade {key}: Volume painted {seen}")
    check(any(paint["icons"] > 0 for paint in lines), "no arrow counted in GridStats::icons")


def uniform_after(app, state):
    """The `paint` lines recorded entirely after the frame of `state`."""
    return [paint for paint in paints(app, app.lines.index(state))
            if paint["since_frames"] >= state["frames"]]


def scenario_paint_arrows_toggle(app):
    # Arrows off on the toolbar: one colour per change kind, no arrow, from
    # the next frame on.
    since = app.mark()
    app.click(*app.control("arrows"))
    off = app.state(lambda s: s["change_paint"] == "uniform", "the uniform paint", since)
    app.move(*app.outside())
    deadline = time.monotonic() + 8
    while True:
        lines = uniform_after(app, off)
        updated = [cell for paint in lines for cell in paint["cells"] if cell[2] == "updated"]
        if len(updated) >= 5:
            break
        if time.monotonic() > deadline:
            raise Fail(f"{len(updated)} updated cells painted after Arrows off")
        time.sleep(0.1)
    check(all(cell[3] is None for cell in updated),
          f"arrows painted after Arrows off: {[c for c in updated if c[3]][:6]}")
    check(all(paint["icons"] == 0 for paint in lines),
          f"GridStats::icons after Arrows off: {[p['icons'] for p in lines]}")
    settle()
    since = app.mark()
    app.click(*app.control("arrows"))
    on = app.state(lambda s: s["change_paint"] == "directional", "the directional paint", since)
    app.move(*app.outside())
    app.wait(lambda line: line["type"] == "paint" and line["since_frames"] >= on["frames"]
             and any(cell[3] for cell in line["cells"]),
             "an arrow painted after Arrows on", app.lines.index(on), timeout=8)
    settle()


# Phase "footers": RATE=0, BLOTTER_GROUP=instrument, BLOTTER_FOOTERS=1
# (EveryGroupFooter), BLOTTER_TOTALS=1.


def check_footers(lines):
    """Every expanded caption among `lines` is followed by its trades and
    a footer with its path; a collapsed one by the next caption. Returns
    the number of footers seen."""
    ix, footers = 0, 0
    while ix < len(lines):
        caption = lines[ix]
        check(caption["kind"] == "caption" and len(caption["path"]) == 1,
              f"line {ix}: {caption}, a caption expected")
        if not caption["expanded"]:
            check(ix + 1 >= len(lines) or lines[ix + 1]["kind"] == "caption",
                  f"collapsed {caption['path']}: line {ix + 1} is {lines[ix + 1]}")
            ix += 1
            continue
        end = ix + 1 + caption["rows"]
        if end >= len(lines):
            break
        trades = lines[ix + 1:end]
        check(all(line["kind"] == "data" for line in trades),
              f"the trades of {caption['path']}: {trades}")
        check(lines[end] == {"kind": "footer", "path": caption["path"]},
              f"line {end} after {caption['path']}: {lines[end]}, its footer expected")
        footers += 1
        ix = end + 1
    return footers


def scenario_footer_lines(app):
    to_top(app)
    state = app.latest("state")
    check(state["group_by"] == [level("instrument")], f"grouping {state['group_by']}")
    check(check_footers(state["lines"]) >= 10, "fewer than ten footers among the first lines")
    first = state["lines"][0]
    # A collapsed group shows no footer.
    since = app.mark()
    app.click(*app.chevron(first["path"]))
    app.event("ExpansionChange", since, "the first group collapsed", origin="User")
    state = app.state(lambda s: not s["lines"][0]["expanded"], "collapsed on screen", since)
    check(not any(line == {"kind": "footer", "path": first["path"]} for line in state["lines"]),
          f"the collapsed {first['path']} kept its footer")
    check_footers(state["lines"])
    settle()
    since = app.mark()
    app.click(*app.chevron(first["path"]))
    state = app.state(lambda s: s["lines"][0]["expanded"], "expanded again", since)
    check(state["lines"][first["rows"] + 1] == {"kind": "footer", "path": first["path"]},
          f"the footer of {first['path']} back: {state['lines'][first['rows'] + 1]}")
    settle()


def first_two_groups(app):
    """The first caption, its last trade's line, its footer's line and the
    second caption, all on screen."""
    to_top(app)
    lines = app.latest("state")["lines"]
    first = lines[0]
    check(first["kind"] == "caption" and first["expanded"], f"first line {first}")
    last = first["rows"]
    footer, second = lines[last + 1], lines[last + 2]
    check(footer["kind"] == "footer" and second["kind"] == "caption",
          f"lines {last + 1}, {last + 2}: {footer}, {second}")
    return first, lines[last], last, second


def scenario_footer_arrows(app):
    first, trade, last, second = first_two_groups(app)
    since = app.mark()
    app.click(app.column_x(), app.row_y(last))
    app.state(lambda s: (s["focus"] or {}).get("key") == trade["key"],
              f"the last trade of {first['path']} focused", since)
    settle()
    # ↓ steps over the footer to the next caption; ↑ back to the trade.
    since = app.mark()
    app.key("Down")
    focus = focus_after(app, since, "Down over the footer")
    check(focus["group"] == second["path"], f"Down: focus {focus}, expected {second['path']}")
    since = app.mark()
    app.key("Up")
    focus = focus_after(app, since, "Up over the footer")
    check(focus["key"] == trade["key"] and focus["group"] is None,
          f"Up: focus {focus}, expected trade {trade['key']}")
    settle()


def scenario_footer_click(app):
    first, _, last, _ = first_two_groups(app)
    before = app.latest("state")
    since = app.mark()
    app.click(app.column_x("volume"), app.row_y(last + 1))
    click = app.event("Click", since, "the footer's Click")
    check_click(click, "group_footer", path=first["path"], column="volume")
    settle()
    for name in ("SelectionChange", "FocusChange"):
        check(not app.events(since, name), f"a click on a footer gave {name}")
    state = app.latest("state")
    check(state["selection"] == before["selection"] and state["focus"] == before["focus"],
          "the selection or the focus moved")


# Phase "fade": RATE=0, BLOTTER_FADE=3000; phase "fade_churn": CHURN=20,
# BLOTTER_FADE=800.


def no_fading(app, timeout=6.0):
    """Waits until no line fades out (the last state and the last frame)."""
    deadline = time.monotonic() + timeout
    while True:
        state = app.latest("state")
        painted = paints(app, 0)
        if (not any(line.get("fading") for line in state["lines"])
                and (not painted or painted[-1]["fading_lines"] == 0)):
            return state
        if time.monotonic() > deadline:
            raise Fail("a line still fades out")
        time.sleep(0.1)


def scenario_fade_ghost(app):
    state = no_fading(app)
    check(state["visible_rows"][0] == 0, f"scrolled: {state['visible_rows']}")
    keys = [line.get("key") for line in state["lines"]]
    ix = keys.index(state["visible"][3])
    after = keys[ix + 1]
    key, since = cancel_trade(app, ix)
    check(key == keys[ix], f"cancelled {key}, line {ix} is {keys[ix]}")
    # It fades out in its place, painted removed; not a row of the grid.
    ghost = app.state(lambda s: s["lines"][ix] == {"kind": "data", "key": key, "fading": True},
                      "the cancelled trade fading in its place", since)
    check(key not in ghost["visible"], "a fading trade among the visible rows")
    app.wait(lambda line: line["type"] == "paint" and key in line["ghosts"]
             and line["fading_lines"] >= 1, "the fading line painted", since)
    # A click on it lands on empty space.
    since = app.mark()
    app.click(app.column_x("volume"), app.row_y(ix))
    click = app.event("Click", since, "the click on the fading line")
    check_click(click, "empty")
    settle()
    for name in ("SelectionChange", "FocusChange", "Confirm"):
        check(not app.events(since, name), f"a click on a fading line gave {name}")
    # Then it goes, and the trades below move up.
    gone = app.state(lambda s: s["lines"][ix].get("key") == after
                     and all(line.get("key") is not None for line in s["lines"]),
                     "the fading line gone", since, timeout=6)
    check(gone["store_rows"] == state["store_rows"] - 1,
          f"{gone['store_rows']} trades in the store, {state['store_rows']} before")
    app.wait(lambda line: line["type"] == "paint" and line["fading_lines"] == 0,
             "no fading line painted", app.lines.index(gone))


def scenario_fade_churn(app):
    # CHURN replaces 20 trades a second: each removed one fades out for
    # 0.8 s in its place, then goes.
    since = app.mark()
    time.sleep(4)
    app.read()
    states = [line for line in app.lines[since:] if line["type"] == "state"]
    painted = paints(app, since)
    fading = {}
    for state in states:
        for line in state["lines"]:
            if line.get("fading"):
                fading.setdefault(line["key"], []).append(state["t"])
                check(line["key"] not in state["visible"],
                      f"fading trade {line['key']} among the visible rows")
    check(len(fading) >= 3, f"{len(fading)} trades seen fading in 4 s")
    for key, times in fading.items():
        check(times[-1] - times[0] <= 1500, f"trade {key} faded for {times[-1] - times[0]:.0f} ms")
    check(any(paint["fading_lines"] > 0 and paint["ghosts"] for paint in painted),
          "no fading line painted (GridStats::fading_lines)")
    # A trade painted fading is gone for good: no later frame shows it as
    # a row (keys are never reused).
    painted_at = {}
    for paint in painted:
        for key in paint["ghosts"]:
            painted_at.setdefault(key, paint["t"])
    for state in states:
        back = [key for key in state["visible"] if painted_at.get(key, state["t"]) < state["t"]]
        check(not back, f"trades painted fading shown as rows again: {back[:5]}")


# Phase "groups_live": summary stamps (`group.stamps`).


def scenario_group_stamps(app):
    # Aggregates change under the stream: their cells on the captions and
    # the totals light up (aggregate_style sees the change), and a
    # caption whose row count changed is stamped itself.
    app.move(*app.outside())
    since = app.mark()
    deadline = time.monotonic() + 8
    while True:
        app.read()
        painted = [entry for paint in paints(app, since) for entry in paint["aggregates"]]
        captions = [entry for entry in painted if entry[0] == "caption"]
        totals = [entry for entry in painted if entry[0] == "totals"]
        states = [line for line in app.lines[since:] if line["type"] == "state"]
        stamped = {tuple(line["path"]) for state in states for line in state["lines"]
                   if line["kind"] == "caption" and line.get("stamp")}
        hot = any(state.get("totals") and state["totals"]["hot"] for state in states)
        # A change of Sum(Volume) between two states.
        totalled = [state for state in states if state.get("totals")]
        changed = next((b for a, b in zip(totalled, totalled[1:])
                        if a["totals"]["values"]["volume"] != b["totals"]["values"]["volume"]),
                       None)
        if (captions and totals and stamped and hot and changed
                and any(entry[4] for entry in painted)):
            break
        if time.monotonic() > deadline:
            raise Fail(f"in 8 s: {len(captions)} caption and {len(totals)} totals cells "
                       f"painted, captions stamped {stamped}, totals hot {hot}, "
                       f"Sum(Volume) changed: {changed is not None}")
        time.sleep(0.1)
    check(all(entry[1] in (["Buy"], ["Sell"]) for entry in captions),
          f"captions painted {captions[:4]}")
    check(all(entry[3] == "updated" for entry in captions + totals),
          f"changes {[entry[3] for entry in captions + totals]}")
    # After the change of Sum(Volume) the totals cell is painted hot.
    app.wait(lambda line: line["type"] == "paint" and any(
        entry[0] == "totals" and entry[2] == "volume" for entry in line["aggregates"]),
        "the totals' Volume painted after its change", app.lines.index(changed))


# Phase "groups": the group panel by keyboard (`group.panel_keys`).


def wait_focus(app, predicate, what, timeout=5.0):
    """Waits until the last `focus` line matches."""
    deadline = time.monotonic() + timeout
    while True:
        try:
            focus = app.latest("focus")
        except Fail:
            focus = None
        if focus is not None and predicate(focus):
            return focus
        if time.monotonic() > deadline:
            raise Fail(f"timed out waiting for {what}; focus {focus}")
        time.sleep(0.02)


def chip_focused(column):
    return lambda focus: not focus["grid"] and (focus["chips"] or {}).get(column) is True


def nowhere(focus):
    """Neither the grid nor a chip: a chip's remove button, or outside."""
    return not focus["grid"] and not any((focus["chips"] or {}).values())


def scenario_group_panel_keys(app):
    # Two levels, Instrument after Side; the keyboard in the grid.
    chips, panel = app.panel()["chips"], app.panel()
    since = app.mark()
    drop_on_panel(app, "instrument", min(chips["end"] + 60, panel["x"] + panel["w"] - 20))
    app.state(lambda s: s["group_by"] == [level("side"), level("instrument")],
              "grouped by Side, then Instrument", since)
    settle()
    to_top(app)
    app.click_row(2)
    wait_focus(app, lambda focus: focus["grid"], "the grid focused")
    settle()
    columns = [c["id"] for c in app.latest("state")["columns"]]
    start = app.mark()
    # Tab: from the grid to the first chip.
    app.key("Tab")
    wait_focus(app, chip_focused("side"), "the Side chip focused by Tab")
    # Enter and Space press it: its groups reverse, and back.
    for key, direction in (("Return", "desc"), ("space", "asc")):
        since = app.mark()
        app.key(key)
        app.event("GroupChange", since, f"{key} on the chip", origin="User")
        app.state(lambda s: s["group_by"] == [level("side", direction), level("instrument")],
                  f"Side {direction} after {key}", since)
        wait_focus(app, chip_focused("side"), f"the focus on the chip after {key}")
    # Tab walks the chips and their remove buttons, Shift+Tab back.
    app.key("Tab")
    wait_focus(app, nowhere, "the focus on the Side chip's remove button")
    app.key("Tab")
    wait_focus(app, chip_focused("instrument"), "the Instrument chip focused by Tab")
    app.key("shift+Tab", "shift+Tab")
    wait_focus(app, chip_focused("side"), "the Side chip focused by Shift+Tab")
    # Delete ungroups its level; the focus moves to the chip on the right.
    since = app.mark()
    app.key("Delete")
    app.event("GroupChange", since, "Delete on the chip", origin="User")
    app.state(lambda s: s["group_by"] == [level("instrument")], "Side ungrouped", since)
    wait_focus(app, chip_focused("instrument"), "the focus on the Instrument chip")
    # Space on its remove button removes the last level.
    app.key("Tab")
    wait_focus(app, nowhere, "the focus on the Instrument chip's remove button")
    since = app.mark()
    app.key("space")
    app.event("GroupChange", since, "Space on the remove button", origin="User")
    app.state(lambda s: s["group_by"] == [], "nothing grouped", since)
    settle()
    check(not app.events(start, "LayoutChange"), "a key on a chip moved a column")
    check([c["id"] for c in app.latest("state")["columns"]] == columns, "the columns moved")


# The tree of `portfolio` (`tree.*`): 2 funds, 8 desks, 64 books and 200
# positions, by name; the funds and desks expanded, the books collapsed.


class Portfolio(Blotter):
    """One `portfolio` process and its log."""

    BIN = PORTFOLIO_BIN
    LOG = "PORTFOLIO_E2E_LOG"
    DEFAULTS = dict(ROWS=ROWS, RATE=0, CHURN=0, PORTFOLIO_MOVES=0)

    def ready(self, state):
        return state["rows"] > 0 and state["visible"]

    def outside(self):
        """A point on the toolbar right of its buttons, off the grid."""
        books = self.geometry()["controls"]["books"]
        return books["x"] + books["w"] + 20, books["y"] + books["h"] / 2

    def line_y(self, ix):
        return self.row_y(ix - self.latest("state")["visible_rows"][0])

    def tree_text(self, ix):
        """A point on the Name cell of display line `ix`, right of the
        chevron lane of any depth."""
        return self.geometry()["tree_x"] + 120, self.line_y(ix)

    def tree_chevron(self, key):
        found = [b for b in self.geometry()["branches"] if b["key"] == key]
        check(found, f"no chevron of row {key} on screen")
        branch = found[0]
        return branch["chevron"]["x"] + branch["chevron"]["w"] / 2, branch["y"] + branch["h"] / 2


def tree_rows(app):
    """Every row of the portfolio by key (the `rows` line, RATE=0)."""
    rows = app.latest("rows")
    return {row[0]: dict(zip(rows["columns"], row)) for row in rows["rows"]}


def line_of(state, name):
    lines = [line.get("name") for line in state["lines"]]
    check(name in lines, f"{name} is not among the lines")
    return lines.index(name)


def positions_under(rows, key):
    """The positions below the row `key`, at any depth."""
    def ancestors(row):
        while row["parent"] is not None:
            yield row["parent"]
            row = rows[row["parent"]]
    return sorted(k for k, row in rows.items()
                  if row["level"] == "position" and key in ancestors(row))


def scenario_tree_startup(app):
    state = app.latest("state")
    check(state["store_rows"] == ROWS + 74, f"{state['store_rows']} rows")
    check(state["rows"] == 74, f"{state['rows']} lines: funds, desks and books expected")
    first = state["lines"][:3]
    check([(line["name"], line["depth"], line["expanded"]) for line in first]
          == [("Fund 1", 0, True), ("Desk 1A", 1, True), ("Book 1A-1", 2, False)],
          f"first lines {first}")
    check(all(line["level"] != "position" for line in state["lines"]), "a position shown")
    # The totals are over the positions (tree.totals); their row count is
    # the view's live rows, as the engine's tests have it.
    check(state["totals"]["rows"] == state["row_count"],
          f"the totals count {state['totals']['rows']} rows of {state['row_count']}")
    check(app.geometry()["totals"], "no totals row in the geometry")


def scenario_tree_totals(app):
    # The totals add up the positions, the leaves: the sums the producer
    # writes on books, desks and funds are not counted again.
    rows = tree_rows(app).values()
    totals = app.latest("state")["totals"]
    for column in ("pnl", "delta"):
        leaves = sum(row[column] for row in rows if row["level"] == "position")
        every = sum(row[column] for row in rows)
        check(abs(totals[column] - leaves) <= 1e-6 * max(1.0, abs(leaves)),
              f"Sum({column}) {totals[column]}, the positions {leaves}")
        check(abs(every - leaves) > 1.0, f"{column}: the tree rows sum to nothing ({every})")


def scenario_tree_chevron(app):
    state = app.latest("state")
    book, other = state["lines"][2], state["lines"][3]
    # A press on a book's text selects and focuses it: a tree node is a row.
    since = app.mark()
    app.click(*app.tree_text(2))
    check(selection_after(app, since, "a press on Book 1A-1") == [book["key"]], "book selected")
    app.state(lambda s: (s["focus"] or {}).get("key") == book["key"], "the book focused", since)
    settle()
    # Another book's chevron expands it, then collapses it: the selection
    # and the focus stay, and a chevron is no Click.
    for expanded in (True, False):
        since = app.mark()
        app.click(*app.tree_chevron(other["key"]))
        change = app.event("ExpansionChange", since, "a press on the chevron", origin="User")
        check(change["target"] == {"row": other["key"]}, f"target {change['target']}")
        app.state(lambda s: s["lines"][3]["expanded"] == expanded
                  and (s["lines"][4]["level"] == "position") == expanded,
                  f"Book 1A-2 {'expanded' if expanded else 'collapsed'}", since)
        settle()
        for name in ("SelectionChange", "FocusChange", "Click"):
            check(not app.events(since, name), f"a press on a chevron gave {name}")
    # Alt on Desk 1B's chevron: the desk and everything below it, one event
    # each time; the focus stays on the book.
    desk = line_of(app.latest("state"), "Desk 1B")
    key = app.latest("state")["lines"][desk]["key"]
    for expanded in (False, True, False):
        since = app.mark()
        app.click(*app.tree_chevron(key), mods=["alt"])
        app.event("ExpansionChange", since, "Alt on the desk's chevron", origin="User")
        state = app.state(lambda s: s["lines"][desk]["expanded"] == expanded,
                          f"Desk 1B {'expanded' if expanded else 'collapsed'}", since)
        settle()
        check(len(app.events(since, "ExpansionChange")) == 1, "one ExpansionChange per press")
        for name in ("SelectionChange", "FocusChange", "Click"):
            check(not app.events(since, name), f"Alt on a chevron gave {name}")
        below = state["lines"][desk + 1]
        if expanded:
            check(below["name"] == "Book 1B-1" and below["expanded"],
                  f"under Desk 1B: {below}, its books expanded too")
    # A plain press: the desk alone, its books collapsed (the start).
    since = app.mark()
    app.click(*app.tree_chevron(key))
    state = app.state(lambda s: s["lines"][desk]["expanded"], "Desk 1B expanded", since)
    check(not state["lines"][desk + 1]["expanded"], f"Book 1B-1 {state['lines'][desk + 1]}")
    check(state["focus"]["key"] == book["key"] and state["selection"]["keys"] == [book["key"]],
          f"focus {state['focus']}, selection {state['selection']}")
    settle()


def tree_key(app, key, what, expansion=None, focus=None):
    """Presses `key`; with `expansion` waits for its ExpansionChange of
    that row, with `focus` for the focus on that row."""
    since = app.mark()
    app.key(key)
    if expansion is not None:
        change = app.event("ExpansionChange", since, what, origin="User")
        check(change["target"] == {"row": expansion}, f"{what}: target {change['target']}")
    if focus is not None:
        app.state(lambda s: (s["focus"] or {}).get("key") == focus, f"{what}: focus", since)
    return since


def scenario_tree_keys(app):
    state = app.latest("state")
    desk, book = state["lines"][1]["key"], state["lines"][2]["key"]
    since = app.mark()
    app.click(*app.tree_text(2))
    app.state(lambda s: (s["focus"] or {}).get("key") == book, "Book 1A-1 focused", since)
    settle()
    # `+` expands a collapsed book; on an expanded one it does nothing.
    since = tree_key(app, "plus", "+ on the book", expansion=book)
    app.state(lambda s: s["lines"][2]["expanded"] and s["lines"][3]["level"] == "position",
              "the book's positions shown", since)
    since = tree_key(app, "plus", "+ on the expanded book")
    settle()
    check(not app.events(since, "ExpansionChange"), "+ on an expanded book changed it")
    # `-` collapses it; `-` on a collapsed book collapses its desk and goes
    # there (WAI-ARIA treegrid, "move to parent"). The keys walk the lines
    # shown: each waits for the last expansion on screen.
    since = tree_key(app, "minus", "- on the book", expansion=book)
    app.state(lambda s: not s["lines"][2]["expanded"] and s["lines"][3]["level"] == "book",
              "the book collapsed", since)
    since = tree_key(app, "minus", "- on the collapsed book", expansion=desk, focus=desk)
    app.state(lambda s: not s["lines"][1]["expanded"] and s["lines"][2]["name"] == "Desk 1B",
              "Desk 1A collapsed", since)
    since = tree_key(app, "plus", "+ on the desk", expansion=desk)
    app.state(lambda s: s["lines"][1]["expanded"] and s["lines"][2]["key"] == book,
              "Desk 1A expanded", since)
    # `-` on a position collapses its book and goes there.
    tree_key(app, "Down", "Down to the book", focus=book)
    since = tree_key(app, "plus", "+ on the book again", expansion=book)
    position = app.state(lambda s: s["lines"][3]["level"] == "position", "positions", since)
    leaf = position["lines"][3]["key"]
    tree_key(app, "Down", "Down to the position", focus=leaf)
    since = tree_key(app, "minus", "- on the position", expansion=book, focus=book)
    app.state(lambda s: not s["lines"][2]["expanded"], "the book collapsed again", since)
    # ← and → move between cells; they expand nothing.
    since = app.mark()
    app.key("Right")
    focus = focus_after(app, since, "Right")
    check(focus["key"] == book and focus["column"] == "instrument", f"Right: {focus}")
    since2 = app.mark()
    app.key("Left")
    focus = focus_after(app, since2, "Left")
    check(focus["key"] == book and focus["column"] == "name", f"Left: {focus}")
    settle()
    check(not app.events(since, "ExpansionChange"), "Left or Right expanded or collapsed")


def scenario_tree_select(app):
    # Tree rows are rows: Cmd/Ctrl+A selects every line shown, books and
    # desks too, no position of a collapsed book.
    state = app.latest("state")
    since = app.mark()
    app.key("ctrl+a")
    change = app.event("SelectionChange", since, "Ctrl+A")
    check(change["len"] == state["rows"], f"Ctrl+A selected {change['len']} of {state['rows']} lines")
    settle()
    # A drag over books selects them. (A press on a selected row waits for
    # the release: clear the selection first.)
    since = app.mark()
    app.key("Escape")
    app.event("SelectionChange", since, "Escape")
    settle()
    x = app.tree_text(2)[0]
    since = app.mark()
    app.drag((x, app.line_y(2)), (x, app.line_y(5)), steps=8, pause=0.05)
    keys = selection_after(app, since, "the drag over four books")
    check(keys == sorted(line["key"] for line in state["lines"][2:6]), f"drag: {keys}")
    settle()
    check(len(app.events(since, "SelectionChange")) == 1, "one SelectionChange per drag")


def scenario_tree_filter(app):
    # Every position shown (Books), then the quick filter: the matching
    # positions and their ancestors for context, nothing else.
    since = app.mark()
    app.click(*app.control("books"))
    app.state(lambda s: s["rows"] == ROWS + 74, "every position shown", since)
    settle()
    rows = tree_rows(app)
    since = app.mark()
    app.key("ctrl+f")
    time.sleep(0.3)
    app.type("NVDA")
    app.state(lambda s: s["filter"] == "NVDA" and s["rows"] < ROWS + 74, "filtered", since)
    settle(1.0)
    state = app.latest("state")
    matching = {key for key, row in rows.items() if row["instrument"] == "NVDA"}
    check(matching, "no NVDA position")
    expected = set(matching)
    for key in matching:
        row = rows[key]
        while row["parent"] is not None:
            expected.add(row["parent"])
            row = rows[row["parent"]]
    shown = [line["key"] for line in state["lines"]]
    check(len(shown) == state["rows"] < 120, f"{state['rows']} lines filtered")
    check(set(shown) == expected and len(shown) == len(expected),
          f"filtered lines: {len(shown)}, expected {len(expected)}; "
          f"extra {sorted(set(shown) - expected)[:5]}, missing {sorted(expected - set(shown))[:5]}")
    # Each ancestor comes before its rows, one depth up.
    for line, below in zip(state["lines"], state["lines"][1:]):
        check(below["depth"] <= line["depth"] + 1, f"{below} under {line}")
    since = app.mark()
    app.key("ctrl+a", "BackSpace")
    app.state(lambda s: s["filter"] == "" and s["rows"] == ROWS + 74, "the filter cleared", since)
    app.click(*app.tree_text(0))
    settle()


# Phase "tree_branches": PORTFOLIO_SELECT=branches (FocusBranches).


def scenario_tree_focus_branches(app):
    rows = tree_rows(app)
    state = app.latest("state")
    desk, book = state["lines"][1]["key"], state["lines"][2]["key"]
    # A press on a book focuses it, it does not select it.
    since = app.mark()
    app.click(*app.tree_text(2))
    app.state(lambda s: (s["focus"] or {}).get("key") == book, "the book focused", since)
    settle()
    check(not app.events(since, "SelectionChange"), "a press on a book selected")
    check(app.latest("state")["selection"]["len"] == 0, "a book in the selection")
    # Space on it does nothing; Enter opens its positions.
    since = app.mark()
    app.key("space")
    settle()
    check(not app.events(since, "SelectionChange"), "Space on a book selected")
    since = app.mark()
    app.key("Return")
    confirm = app.event("Confirm", since, "Enter on the book")
    check(sorted(confirm["keys"]) == positions_under(rows, book),
          f"Enter on the book: {sorted(confirm['keys'])}, its positions {positions_under(rows, book)}")
    # On the desk: every position of its books.
    since = app.mark()
    app.click(*app.tree_text(1))
    app.state(lambda s: (s["focus"] or {}).get("key") == desk, "the desk focused", since)
    settle()
    since = app.mark()
    app.key("Return")
    confirm = app.event("Confirm", since, "Enter on the desk")
    check(sorted(confirm["keys"]) == positions_under(rows, desk),
          f"Enter on the desk: {len(confirm['keys'])} positions, "
          f"expected {len(positions_under(rows, desk))}")
    # Cmd/Ctrl+A selects the positions shown: the expanded book's.
    since = app.mark()
    app.click(*app.tree_chevron(book))
    app.state(lambda s: s["lines"][2]["expanded"], "the book expanded", since)
    settle()
    since = app.mark()
    app.key("ctrl+a")
    change = app.event("SelectionChange", since, "Ctrl+A")
    check(change["keys"] == positions_under(rows, book), f"Ctrl+A: {change['keys']}")


# Phase "tree_live": RATE=50, CHURN=20, PORTFOLIO_MOVES=2.


def scenario_tree_live(app):
    state = app.latest("state")
    book = state["lines"][2]["key"]
    since = app.mark()
    app.click(*app.tree_text(2))
    app.state(lambda s: (s["focus"] or {}).get("key") == book, "Book 1A-1 focused", since)
    app.move(*app.outside())
    since = tree_key(app, "plus", "+ on the book", expansion=book)
    start = app.state(lambda s: s["lines"][2]["expanded"], "the book expanded", since)
    time.sleep(3)
    app.read()
    states = [line for line in app.lines[app.lines.index(start):] if line["type"] == "state"]
    children, pnl = set(), set()
    for state in states:
        branches = [line for line in state["lines"] if "children" in line]
        mine = [line for line in branches if line["key"] == book]
        check(mine and mine[0]["expanded"], f"the expanded book: {mine}")
        check(state["focus"]["key"] == book, f"the focus moved: {state['focus']}")
        for line in branches:
            if line["key"] != book:
                check(line["expanded"] == (line["level"] != "book"),
                      f"{line['name']} {'expanded' if line['expanded'] else 'collapsed'}")
        shown = [line for line in state["lines"] if line.get("level") == "position"]
        check(all(line["depth"] == 3 for line in shown), "positions of another book shown")
        children.add(tuple(line["children"] for line in branches if line["level"] == "book"))
        pnl.add(state["totals"]["pnl"])
    check(len(states) > 5, f"{len(states)} states in 3 s")
    check(len(children) > 1, "no book got or lost a position in 3 s")
    check(len(pnl) > 1, "the total P&L stayed")
    since = tree_key(app, "minus", "- on the book", expansion=book)


# `dock_layout` (`dock.*`): two trades tabs, the second grouped by
# instrument and never shown; a restart restores its grouping from the file
# alone (the dock registry builds every panel ungrouped).


class Dock(Blotter):
    """One `dock_layout` process and its log."""

    BIN = DOCK_BIN
    LOG = "DOCK_E2E_LOG"
    DEFAULTS = {}

    def ready(self, state):
        # A tab never shown pulls no snapshot: it has no lines.
        return len(state["grids"]) == 2 and state["grids"][0]["rows"] > 0

    def start(self):
        self.state(self.ready, "both trades grids loaded", timeout=30)
        return self


def scenario_dock_saved(app, path):
    # The first start: the dock of `Workspace::restore`, its second tab
    # grouped by instrument behind the first. Quitting saves it (the quit
    # file, as closing the window).
    state = app.state(lambda s: s["grids"][0]["frames"] > 0, "the first tab painted")
    grids = state["grids"]
    check([grid["group_by"] for grid in grids] == [[], ["instrument"]],
          f"grouping of the tabs {[grid['group_by'] for grid in grids]}")
    check(grids[1]["frames"] == 0, "the second tab was shown")


def scenario_dock_restored(app, path):
    text = path.read_text() if path.exists() else ""
    check('"instrument"' in text and "group_by" in text, "the dock file has no grouping")
    grids = app.latest("state")["grids"]
    check([grid["group_by"] for grid in grids] == [[], ["instrument"]],
          f"grouping after the restart {[grid['group_by'] for grid in grids]}")
    check(grids[1]["frames"] == 0, "the restored grouped tab was shown")


# The run.


# The orders example (examples/orders), edited in place.


class OrdersApp(Blotter):
    """One `orders` process and its log."""

    BIN = ORDERS_BIN
    LOG = "ORDERS_E2E_LOG"
    DEFAULTS = dict(
        ROWS=ROWS, RATE=0, FILLS=0, ORDERS_REJECT_PERCENT=0,
        ORDERS_CONFLICT_PERCENT=0, ORDERS_LATENCY_MS=200,
        ORDERS_REMOVE_FILLED_MS=0,
    )

    def ready(self, state):
        return state["rows"] > 0 and state["selection"]["len"] == 1 and state["visible"]

    def visible(self):
        return self.latest("state")["visible"]

    def values(self, key):
        """The values of a shown order (the first rows of a state line)."""
        for row in reversed(self.lines):
            if row["type"] == "state":
                for values in row.get("values", []):
                    if values["id"] == key:
                        return values
        raise Fail(f"no values of order {key}")

    def cell(self, key, column):
        """The middle of the cell of `key` and `column` on screen: its line
        among the lines shown, group captions too."""
        lines = self.latest("state")["lines"]
        ix = [ix for ix, line in enumerate(lines) if line.get("key") == key]
        check(ix, f"order {key} is not on screen")
        return self.header(column)[0], self.row_y(ix[0])

    def editor_items(self):
        editor = self.geometry().get("editor") or {}
        return editor.get("items") or {}

    def working(self, count=1, skip=0, where=None):
        """`count` shown orders that take an edit, from the `skip`-th of the
        first 20 shown on (the state lines hold the values of 25)."""
        found = []
        for key in self.visible()[skip:20]:
            values = self.values(key)
            if values["status"] in ("Working", "New") and (where is None or where(values)):
                found.append(key)
                if len(found) == count:
                    return found
        raise Fail("not enough working orders on screen")

    def caption_text(self, path):
        """A point on a caption's text, clear of its chevron."""
        caption = self.caption(path)
        return self.geometry()["grid"]["x"] + 160, caption["y"] + caption["h"] / 2

    def basket_point(self):
        basket = self.geometry()["basket"]
        check(basket, "no basket in the geometry")
        grid = basket["grid"]
        return grid["x"] + grid["w"] / 2, basket["rows_top"] + 3 * basket["row_height"]


def in_band(values, factor=1.01):
    """A limit inside the server's band: `factor` times Last, two decimals."""
    return round(values["last"] * factor, 2)


def edit_states(state, key, column):
    return [s for s in state["edit_states"] if s["key"] == key and s["column"] == column]


def type_into_field(app, text):
    """Replaces the open field's text: select all, then type."""
    app.key("ctrl+a")
    app.type(text)


def scenario_orders_startup(app):
    state = app.latest("state")
    check(state["store_rows"] == ROWS, f"{state['store_rows']} orders")
    check(state["editing"] is None and state["pending"] == 0, "no edit at start")
    geometry = app.geometry()
    for column in ("id", "limit", "side", "active", "cancel", "status"):
        check(column in geometry["header"]["cells"], f"no header cell {column}")
    check(geometry["basket"], "no basket")


def scenario_edit_type_enter(app):
    key = app.working()[0]
    values = app.values(key)
    limit = in_band(values)
    since = app.mark()
    app.click(*app.cell(key, "limit"))
    settle()
    app.type(f"{limit}")
    app.event("EditStart", since, "EditStart from typing", key=key, column="limit")
    app.key("Return")
    commit = app.event("EditCommit", since, "the commit", key=key, column="limit")
    check(commit["value"] == limit, f"committed {commit['value']}, typed {limit}")
    focus = app.state(lambda s: s["focus"] and s["focus"]["key"] != key, "the focus below", since)
    check(focus["focus"]["column"] == "limit", f"focus {focus['focus']}")
    rounded = round(round(limit / 0.05) * 0.05, 2)
    app.state(
        lambda s: not edit_states(s, key, "limit")
        and any(v["id"] == key and abs(v["limit"] - rounded) < 1e-6 for v in s.get("values", [])),
        f"the store's {rounded} and no edit state",
        since,
    )
    settle()


def scenario_edit_f2_escape(app):
    key = app.working(skip=1)[0]
    before = app.values(key)["quantity"]
    app.click(*app.cell(key, "quantity"))
    settle()
    since = app.mark()
    app.key("F2")
    app.event("EditStart", since, "EditStart from F2", key=key, column="quantity")
    app.type("7")
    app.key("Escape")
    cancel = app.event("EditCancel", since, "the cancel", key=key)
    check(cancel["reason"] == "Escape", f"cancel {cancel}")
    state = app.state(lambda s: s["editing"] is None, "the field closed", since)
    check(state["focus"]["key"] == key and state["focus"]["column"] == "quantity", f"focus {state['focus']}")
    check(app.values(key)["quantity"] == before, "the value stayed")
    settle()


def scenario_edit_tab_flow(app):
    key = app.working(skip=2)[0]
    app.click(*app.cell(key, "quantity"))
    settle()
    since = app.mark()
    app.key("F2")
    type_into_field(app, "200")
    app.key("Tab")
    commit = app.event("EditCommit", since, "the commit by Tab", key=key, column="quantity")
    check(commit["value"] == 200, f"committed {commit}")
    app.state(lambda s: s["focus"]["column"] == "limit" and s["editing"] is None, "focus on Limit", since)
    since = app.mark()
    app.key("F2")
    app.event("EditStart", since, "the limit's field", key=key, column="limit")
    app.key("shift+Tab")
    app.state(
        lambda s: s["focus"]["column"] == "quantity" and s["editing"] is None,
        "focus back on Quantity", since,
    )
    settle()


def scenario_edit_invalid(app):
    key = app.working(skip=3)[0]
    app.click(*app.cell(key, "quantity"))
    settle()
    since = app.mark()
    app.key("BackSpace")
    app.type("150")
    app.key("Return")
    state = app.state(lambda s: s["mark"] == "invalid", "the invalid mark", since)
    check(state["editing"] == {"key": key, "column": "quantity"}, f"editing {state['editing']}")
    check("multiple of 100" in (state["message"] or ""), f"message {state['message']}")
    check(not app.events(since, "EditCommit"), "no commit of 150")
    type_into_field(app, "200")
    app.key("Return")
    commit = app.event("EditCommit", since, "the commit of 200", key=key, column="quantity")
    check(commit["value"] == 200, f"committed {commit}")
    settle()


def scenario_edit_click_away(app):
    key, other = app.working(count=2, skip=4)
    app.click(*app.cell(key, "note"))
    settle()
    since = app.mark()
    app.key("F2")
    app.type(" e2e")
    app.click(*app.cell(other, "instrument"))
    commit = app.event("EditCommit", since, "the commit", key=key, column="note")
    selection = app.event("SelectionChange", since, "the new selection")
    focus = app.event("FocusChange", since, "the new focus")
    check(commit["seq"] < selection["seq"] and commit["seq"] < focus["seq"],
          "the commit before the selection and the focus")
    settle()


def scenario_edit_select(app):
    key = app.working(skip=5, where=lambda v: v["tif"] == "Day")[0]
    app.click(*app.cell(key, "tif"))
    settle()
    since = app.mark()
    app.key("F2")
    app.event("EditStart", since, "the list", key=key, column="tif")
    app.wait(lambda line: line["type"] == "geometry" and "GTC" in ((line.get("editor") or {}).get("items") or {}),
             "the list in the geometry", since)
    app.key("Down")
    app.key("Return")
    commit = app.event("EditCommit", since, "the commit", key=key, column="tif")
    check(commit["value"] == "GTC", f"committed {commit}")
    settle(0.4)
    check(len(app.events(since, "EditCommit")) == 1, "one commit")
    check(not app.events(since, "Confirm"), "no Confirm of the grid")
    settle()


def scenario_edit_date(app):
    key = app.working(skip=6)[0]
    expiry = app.values(key)["expiry"][:10]
    app.click(*app.cell(key, "expiry"))
    settle()
    since = app.mark()
    app.key("F2")
    app.event("EditStart", since, "the calendar", key=key, column="expiry")
    line = app.wait(
        lambda line: line["type"] == "geometry"
        and any(len(name) == 10 and name[4] == "-" for name in ((line.get("editor") or {}).get("items") or {})),
        "the calendar in the geometry", since)
    days = sorted(name for name in line["editor"]["items"] if len(name) == 10 and name[4] == "-")
    later = [day for day in days if day > expiry]
    check(later, f"no day after {expiry} on the calendar")
    day = later[0]
    app.click(*center(line["editor"]["items"][day]))
    commit = app.event("EditCommit", since, "the commit of a day", key=key, column="expiry")
    check(commit["value"].startswith(f"{day}T00:00:00"), f"committed {commit['value']}")
    settle()


def scenario_edit_reject(app):
    key = app.working(skip=7)[0]
    before = app.values(key)["limit"]
    app.click(*app.cell(key, "limit"))
    settle()
    since = app.mark()
    app.key("BackSpace")
    app.type("999")
    app.key("Return")
    app.event("EditCommit", since, "the commit of 999", key=key, column="limit")
    state = app.state(
        lambda s: any(e.get("state") == "failed" for e in edit_states(s, key, "limit")),
        "the failed mark", since, timeout=5)
    failed = edit_states(state, key, "limit")[0]
    check(failed["message"] == "Price out of band", f"failed {failed}")
    check(app.values(key)["limit"] == before, "the store's value again")
    app.click(*app.cell(key, "limit"))
    settle()
    since = app.mark()
    app.key("F2")
    app.state(lambda s: not edit_states(s, key, "limit"), "F2 took the mark off", since)
    app.key("Escape")
    settle()


def scenario_widget_checkbox(app):
    key = app.working(skip=8)[0]
    active = app.values(key)["active"]
    since = app.mark()
    app.click(*app.cell(key, "active"))
    commit = app.event("EditCommit", since, "the checkbox's commit", key=key, column="active")
    check(commit["value"] == (not active), f"committed {commit}")
    selection = app.event("SelectionChange", since, "the click selected the row")
    check(selection["keys"] == [key], f"selection {selection}")
    settle()
    since = app.mark()
    app.key("space")
    commit = app.event("EditCommit", since, "Space's commit", key=key, column="active")
    check(commit["value"] == active, f"committed {commit}")
    since = app.mark()
    app.key("Down")
    app.state(lambda s: s["focus"]["key"] != key, "Down moved the grid's focus", since)
    settle()


def scenario_widget_button_stops(app):
    state = app.latest("state")
    selected = set(state["selection"]["keys"])
    key = app.working(skip=9, where=lambda v: v["id"] not in selected)[0]
    since = app.mark()
    app.click(*app.cell(key, "cancel"))
    app.state(
        lambda s: any(v["id"] == key and v["status"] == "Cancelled" for v in s.get("values", [])),
        "the order cancelled", since)
    settle()
    check(not app.events(since, "SelectionChange"), "no selection change")
    check(not app.events(since, "Click"), "no Click")
    app.click(*app.cell(key, "limit"))
    settle()
    since = app.mark()
    app.key("F2")
    settle()
    check(not app.events(since, "EditStart"), "a cancelled order takes no edit")


def scenario_widget_double_click(app):
    key = app.working(skip=10)[0]
    since = app.mark()
    app.click(*app.cell(key, "side"), repeat=2)
    app.event("EditStart", since, "the list of Side", key=key, column="side")
    app.key("Escape")
    settle()
    since = app.mark()
    app.click(*app.cell(key, "id"), repeat=2)
    confirm = app.event("Confirm", since, "Confirm on Id")
    check(confirm["keys"] == [key], f"confirm {confirm}")
    # The example's primary command opens the limit's field.
    app.event("EditStart", since, "Amend order", key=key, column="limit")
    app.key("Escape")
    settle()


def scenario_edit_keys_stay(app):
    key = app.working(skip=11)[0]
    app.click(*app.cell(key, "note"))
    settle()
    before = app.latest("state")
    since = app.mark()
    app.key("F2")
    app.event("EditStart", since, "the note's field", key=key, column="note")
    app.key("Next", "ctrl+a", "shift+F10")
    settle()
    state = app.latest("state")
    check(state["visible_rows"] == before["visible_rows"], "the body did not scroll")
    check(state["selection"]["len"] == before["selection"]["len"], "no rows selected")
    check(not state["menu"], "no grid menu")
    check(state["editing"] == {"key": key, "column": "note"}, "the field stays open")
    app.key("Escape")
    settle()


def scenario_drag_basket(app):
    key = app.working(skip=12)[0]
    app.click(*app.cell(key, "instrument"))
    settle()
    since = app.mark()
    app.drag(app.cell(key, "instrument"), app.basket_point(), steps=16)
    drop = app.event("Drop", since, "the drop on the basket", grid="basket")
    check(drop["source"]["keys"] == [key], f"drop {drop}")
    app.state(lambda s: key in s["basket"]["keys"] and key not in s["visible"], "the order in the basket", since)
    settle()
    # A press on a checkbox drags nothing.
    other = app.working(skip=12)[0]
    app.click(*app.cell(other, "instrument"))
    settle()
    since = app.mark()
    app.drag(app.cell(other, "active"), app.basket_point(), steps=16)
    settle()
    check(not app.events(since, "Drop"), "no drop from a checkbox")


def scenario_drag_account(app):
    state = app.latest("state")
    captions = [line for line in state["lines"] if line["kind"] == "caption"]
    check(len(captions) >= 2, "two account captions on screen")
    first = captions[0]["path"][0]
    rows = [line["key"] for line in state["lines"] if line["kind"] == "data"]
    key = rows[0]
    target = [c for c in captions if c["path"][0] != first][0]["path"]
    app.click(*app.cell(key, "instrument"))
    settle()
    since = app.mark()
    app.drag(app.cell(key, "instrument"), app.caption_text(target), steps=16)
    drop = app.event("Drop", since, "the drop on a caption", grid="orders")
    check(drop["target"] == {"kind": "group", "path": target}, f"drop {drop}")
    app.state(
        lambda s: any(v["id"] == key and v["account"] == target[0] for v in s.get("values", [])),
        f"order {key} in {target[0]}", since)
    settle()


def scenario_export_csv(app, path):
    key = app.working(skip=1)[0]
    limit = in_band(app.values(key), 1.02)
    app.click(*app.cell(key, "limit"))
    settle()
    since = app.mark()
    app.key("BackSpace")
    app.type(f"{limit}")
    app.key("Return")
    app.event("EditCommit", since, "the commit", key=key, column="limit")
    app.click(*app.control("export"))
    deadline = time.monotonic() + 10
    while not path.exists() and time.monotonic() < deadline:
        time.sleep(0.1)
    check(path.exists(), f"no file {path}")
    time.sleep(0.3)
    data = path.read_bytes().decode("utf-8-sig")
    check("\r\n" in data, "CRLF line ends")
    lines = data.split("\r\n")
    if lines and lines[-1] == "":
        lines.pop()
    check(lines[0].startswith("Id,Account,Instrument"), f"header {lines[0]!r}")
    check(len(lines) == 1 + ROWS, f"{len(lines) - 1} rows")
    row = [line for line in lines[1:] if line.split(",")[0] == str(key)]
    check(row, f"no row of order {key}")
    check(f"{limit:.2f}" in row[0], f"the waiting value {limit:.2f} in {row[0]!r}")
    check("Working" in data, "statuses as text")


def scenario_edit_hold(app):
    visible = app.visible()
    key = visible[3]
    app.click(*app.cell(key, "note"))
    settle()
    since = app.mark()
    app.key("F2")
    app.event("EditStart", since, "the field", key=key, column="note")
    app.event("OrderHeldChange", since, "the order held", held=True, timeout=5)
    deadline = time.monotonic() + 3
    seen = since
    while time.monotonic() < deadline:
        time.sleep(0.2)
        app.read()
        for line in app.lines[seen:]:
            if line["type"] == "state":
                check(line["visible"].index(key) == 3 if key in line["visible"] else False,
                      "the edited row stays on its screen line")
        seen = len(app.lines)
    since = app.mark()
    app.key("Return")
    app.event("OrderHeldChange", since, "the order shown", held=False)
    settle()


def scenario_edit_conflict(app):
    key = app.working(skip=2)[0]
    app.click(*app.cell(key, "limit"))
    settle()
    since = app.mark()
    app.key("F2")
    state = app.state(lambda s: s["mark"] == "conflict", "the conflict mark", since)
    check("while you edited" in (state["message"] or ""), f"message {state['message']}")
    type_into_field(app, f"{in_band(app.values(key))}")
    app.key("Return")
    commit = app.event("EditCommit", since, "the commit over a conflict", key=key, column="limit")
    check(commit["base"] != commit["current"], f"base {commit['base']} current {commit['current']}")
    settle()


def scenario_edit_row_removed(app):
    key = app.working(skip=2)[0]
    app.click(*app.cell(key, "limit"))
    settle()
    since = app.mark()
    app.key("F2")
    cancel = app.event("EditCancel", since, "the edit closed", key=key)
    check(cancel["reason"] == "RowRemoved", f"cancel {cancel}")
    state = app.state(lambda s: s["editing"] is None and key not in s["visible"], "the order gone", since)
    check(state["focus"] and state["focus"]["key"] not in (None, key), f"focus {state['focus']}")
    settle()


def scenario_edit_minus_grouped(app):
    key = app.working()[0]
    app.click(*app.cell(key, "quantity"))
    settle()
    since = app.mark()
    app.key("minus", "5", "Return")
    state = app.state(lambda s: s["mark"] == "invalid", "the minimum refused", since)
    check(state["editing"] == {"key": key, "column": "quantity"}, "the field stays open")
    app.key("Escape")
    settle()
    app.click(*app.cell(key, "id"))
    settle()
    since = app.mark()
    app.key("minus")
    app.event("ExpansionChange", since, "the group collapsed")
    settle()


class Runner:
    """Runs the phases. With `dry` it only records the scenario names, so
    the filters can be checked before anything starts."""

    def __init__(self, only, dry=False):
        self.only = only
        self.dry = dry
        self.results = []
        self.known = []

    def wanted(self, name):
        """A filter picks a scenario by its name or a prefix of it."""
        return not self.only or any(name.startswith(prefix) for prefix in self.only)

    def run(self, name, app, scenario, *args, force=False):
        if not force and not self.wanted(name):
            return
        started = time.monotonic()
        try:
            scenario(app, *args)
            verdict, reason = "PASS", ""
        except Skip as skip:
            verdict, reason = "SKIP", str(skip)
        except (Fail, subprocess.SubprocessError, KeyError, IndexError, ValueError) as error:
            verdict, reason = "FAIL", f"{type(error).__name__}: {error}"
            if app is not None and app.proc.poll() is None:
                app.screenshot(f"fail-{name}")
                # Leave no menu open for the next scenario.
                try:
                    app.key("Escape")
                except subprocess.SubprocessError:
                    pass
                settle()
        line = f"{verdict} {name} ({time.monotonic() - started:.1f} s)"
        if reason:
            line += f": {reason}"
        print(line, flush=True)
        self.results.append((verdict, name, reason))

    def phase(self, name, scenarios, app_class=Blotter, feeds=(), when=None, **env):
        """Starts an `app_class` (a blotter by default), runs `scenarios`
        and quits it; the quit and its exit code are a check too. It runs
        if any of its scenarios is wanted. `feeds` names scenarios of a
        later phase that read what this one saves: when one is wanted, all
        of these run too. `when` skips the phase while it returns false."""
        self.known.extend(scenario_name for scenario_name, *_ in scenarios)
        if self.dry:
            return None
        feed = any(self.wanted(feed_name) for feed_name in feeds)
        if not feed and not any(self.wanted(scenario_name) for scenario_name, *_ in scenarios):
            return None
        if when is not None and not when():
            return None
        app = app_class(name, **env)
        try:
            app.start()
        except (Fail, subprocess.SubprocessError) as error:
            app.kill()
            self.results.append(("FAIL", f"{name}.start", str(error)))
            print(f"FAIL {name}.start: {error}", flush=True)
            return None
        for scenario_name, scenario, *args in scenarios:
            self.run(scenario_name, app, scenario, *args, force=feed)
        app.screenshot(f"{name}-end")

        def quit_cleanly(app):
            code = app.quit()
            check(code == 0, f"exit code {code}")

        # Always, whatever scenarios were picked: no blotter is left running.
        only, self.only = self.only, []
        self.run(f"{name}.quit", app, quit_cleanly)
        self.only = only


def phases(runner):
    runner.phase("static", [
        ("static.startup", scenario_startup),
        ("sort.cycle", scenario_sort_cycle),
        ("sort.shift_priority", scenario_sort_shift_priority),
        ("sort.clear_all", scenario_sort_clear_all),
        ("selection.mouse", scenario_selection_mouse),
        ("selection.drag", scenario_selection_drag),
        ("selection.keyboard", scenario_selection_keyboard),
        ("menu.context", scenario_context_menu),
        ("menu.double_click", scenario_double_click),
        ("copy.clipboard", scenario_copy),
        ("filter.quick", scenario_quick_filter),
        ("reload.keeps_selection", scenario_reload_keeps_selection),
        ("reload.selects_first_row", scenario_reload_selects_first_row),
        ("pinned.bands", scenario_pinned_rows),
        ("click.cell", scenario_click_cell),
        ("click.total", scenario_click_total),
        ("click.header", scenario_click_header),
        ("click.empty", scenario_click_empty),
        ("click.double", scenario_click_double),
        ("click.drag", scenario_click_drag),
    ])
    runner.phase("live", [
        ("hold.pointer", scenario_hold_pointer),
        ("hold.expires", scenario_hold_expires),
        ("manual.refresh", scenario_manual_refresh),
        ("live.reload", scenario_reload_live),
        ("live.reload_same_frame", scenario_reload_same_frame),
        ("paint.stream_rises", scenario_paint_stream_rises),
        ("paint.arrows_toggle", scenario_paint_arrows_toggle),
    ], RATE=50, BLOTTER_REORDER="hold")
    layout = OUT / "layout.json"
    layout.unlink(missing_ok=True)
    saved = {}
    runner.phase("layout1", [
        ("columns.gestures", scenario_columns, saved),
    ], feeds=["layout.restored"], BLOTTER_LAYOUT=layout)
    runner.phase("layout2", [
        ("layout.restored", scenario_layout_restored, saved),
    ], when=lambda: bool(saved), BLOTTER_LAYOUT=layout)

    # Groups.
    runner.phase("groups", [
        ("group.panel_drag", scenario_group_panel_drag),
        ("group.chips", scenario_group_chips),
        ("group.chevron", scenario_group_chevron),
        ("group.keyboard", scenario_group_keyboard),
        ("group.focus_not_selection", scenario_group_focus_not_selection),
        ("group.menu", scenario_group_menu),
        ("group.totals", scenario_group_totals),
        ("group.copy", scenario_group_copy),
        ("group.panel_keys", scenario_group_panel_keys),
    ], BLOTTER_TOTALS=1)
    runner.phase("groups_live", [
        ("group.stamps", scenario_group_stamps),
        ("group.live", scenario_group_live),
    ], RATE=50, CHURN=20, BLOTTER_GROUP="side", BLOTTER_TOTALS=1)
    group_layout = OUT / "groups-layout.json"
    group_layout.unlink(missing_ok=True)
    group_saved = {}
    runner.phase("groups_layout1", [
        ("group.layout_saved", scenario_group_layout_saved, group_saved, group_layout),
    ], feeds=["group.layout_restored"], BLOTTER_LAYOUT=group_layout,
        BLOTTER_GROUP="instrument,side")
    runner.phase("groups_layout2", [
        ("group.layout_restored", scenario_group_layout_restored, group_saved),
    ], when=lambda: bool(group_saved), BLOTTER_LAYOUT=group_layout)
    runner.phase("footers", [
        ("footer.lines", scenario_footer_lines),
        ("footer.arrows", scenario_footer_arrows),
        ("footer.click", scenario_footer_click),
    ], BLOTTER_GROUP="instrument", BLOTTER_FOOTERS=1, BLOTTER_TOTALS=1)
    runner.phase("fade", [
        ("paint.falls", scenario_paint_falls),
        ("fade.ghost", scenario_fade_ghost),
    ], BLOTTER_FADE=3000)
    runner.phase("fade_churn", [
        ("fade.churn", scenario_fade_churn),
    ], CHURN=20, BLOTTER_FADE=800)

    # The tree (portfolio).
    runner.phase("tree", [
        ("tree.startup", scenario_tree_startup),
        ("tree.totals", scenario_tree_totals),
        ("tree.chevron", scenario_tree_chevron),
        ("tree.keys", scenario_tree_keys),
        ("tree.select", scenario_tree_select),
        ("tree.filter", scenario_tree_filter),
    ], app_class=Portfolio)
    runner.phase("tree_branches", [
        ("tree.focus_branches", scenario_tree_focus_branches),
    ], app_class=Portfolio, PORTFOLIO_SELECT="branches")
    runner.phase("tree_live", [
        ("tree.live", scenario_tree_live),
    ], app_class=Portfolio, RATE=50, CHURN=20, PORTFOLIO_MOVES=2)

    # A grouped tab never shown, across a restart (dock_layout).
    dock_file = OUT / "dock-layout.json"
    dock_file.unlink(missing_ok=True)
    runner.phase("dock1", [
        ("dock.saved", scenario_dock_saved, dock_file),
    ], app_class=Dock, feeds=["dock.restored"], DOCK_LAYOUT=dock_file)
    runner.phase("dock2", [
        ("dock.restored", scenario_dock_restored, dock_file),
    ], app_class=Dock, when=dock_file.exists, DOCK_LAYOUT=dock_file)

    # The orders example, edited in place.
    runner.phase("orders", [
        ("orders.startup", scenario_orders_startup),
        ("edit.type_enter", scenario_edit_type_enter),
        ("edit.f2_escape", scenario_edit_f2_escape),
        ("edit.tab_flow", scenario_edit_tab_flow),
        ("edit.invalid", scenario_edit_invalid),
        ("edit.click_away", scenario_edit_click_away),
        ("edit.select", scenario_edit_select),
        ("edit.date", scenario_edit_date),
        ("edit.reject", scenario_edit_reject),
        ("widget.checkbox", scenario_widget_checkbox),
        ("widget.button_stops", scenario_widget_button_stops),
        ("widget.double_click", scenario_widget_double_click),
        ("edit.keys_stay", scenario_edit_keys_stay),
        ("drag.basket", scenario_drag_basket),
    ], app_class=OrdersApp)
    export_path = OUT / "orders.csv"
    export_path.unlink(missing_ok=True)
    runner.phase("orders_export", [
        ("export.csv", scenario_export_csv, export_path),
    ], app_class=OrdersApp, ORDERS_LATENCY_MS=5000, ORDERS_EXPORT_PATH=export_path)
    runner.phase("orders_live", [
        ("edit.hold", scenario_edit_hold),
    ], app_class=OrdersApp, RATE=50, ORDERS_REORDER="live", ORDERS_SORT="last-")
    runner.phase("orders_conflict", [
        ("edit.conflict", scenario_edit_conflict),
    ], app_class=OrdersApp, ORDERS_CONFLICT_ON_EDIT=1)
    runner.phase("orders_fill", [
        ("edit.row_removed", scenario_edit_row_removed),
    ], app_class=OrdersApp, ORDERS_FILL_ON_EDIT=1)
    runner.phase("orders_grouped", [
        ("edit.minus_grouped", scenario_edit_minus_grouped),
    ], app_class=OrdersApp, ORDERS_GROUP="side")
    runner.phase("orders_accounts", [
        ("drag.account", scenario_drag_account),
    ], app_class=OrdersApp, ROWS=20, ORDERS_GROUP="account")


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    # A dry pass lists the scenario names, so a filter that selects nothing
    # fails instead of running nothing and passing.
    plan = Runner(sys.argv[1:], dry=True)
    phases(plan)
    selected = [name for name in plan.known if plan.wanted(name)]
    if not selected:
        print(f"no scenario matches {' '.join(sys.argv[1:])!r}; known scenarios:", file=sys.stderr)
        for name in plan.known:
            print(f"  {name}", file=sys.stderr)
        return 2
    runner = Runner(sys.argv[1:])
    phases(runner)

    counts = {verdict: sum(1 for v, _, _ in runner.results if v == verdict)
              for verdict in ("PASS", "FAIL", "SKIP")}
    summary = (f"{len(runner.results)} checks ran: {counts['PASS']} passed, "
               f"{counts['FAIL']} failed, {counts['SKIP']} skipped")
    with open(OUT / "summary.txt", "w") as out:
        for verdict, name, reason in runner.results:
            out.write(f"{verdict} {name}{': ' + reason if reason else ''}\n")
        out.write(summary + "\n")
    print(summary, flush=True)
    return 1 if counts["FAIL"] else 0


if __name__ == "__main__":
    sys.exit(main())
