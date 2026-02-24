"""Tests for the status bar widget."""


def test_status_bar_instantiates():
    from app.tui.widgets.status_bar import StatusBar
    sb = StatusBar()
    assert sb is not None


def test_status_bar_update_spinner():
    from app.tui.widgets.status_bar import StatusBar
    sb = StatusBar()
    sb.update_agent_step("Searching OPTIMADE databases...")
    assert sb.current_step == "Searching OPTIMADE databases..."


def test_status_bar_update_next_step():
    from app.tui.widgets.status_bar import StatusBar
    sb = StatusBar()
    sb.update_next_step("Parse results")
    assert sb.next_step == "Parse results"


def test_status_bar_add_task():
    from app.tui.widgets.status_bar import StatusBar
    sb = StatusBar()
    sb.add_task("Search alloy databases")
    assert len(sb.tasks) == 1
    assert sb.tasks[0]["label"] == "Search alloy databases"
    assert sb.tasks[0]["status"] == "pending"


def test_status_bar_complete_task():
    from app.tui.widgets.status_bar import StatusBar
    sb = StatusBar()
    sb.add_task("Search alloy databases")
    sb.complete_task(0)
    assert sb.tasks[0]["status"] == "done"


def test_status_bar_start_task():
    from app.tui.widgets.status_bar import StatusBar
    sb = StatusBar()
    sb.add_task("Search alloy databases")
    sb.start_task(0)
    assert sb.tasks[0]["status"] == "running"


def test_status_bar_task_ordering():
    from app.tui.widgets.status_bar import StatusBar
    sb = StatusBar()
    sb.add_task("Task A")
    sb.add_task("Task B")
    sb.add_task("Task C")
    sb.complete_task(0)
    sb.start_task(1)
    ordered = sb.get_display_tasks()
    assert ordered[0]["status"] == "running"
    assert ordered[1]["status"] == "pending"
    assert ordered[2]["status"] == "done"


def test_status_bar_truncates_after_max():
    from app.tui.widgets.status_bar import StatusBar
    sb = StatusBar(max_visible_tasks=3)
    for i in range(10):
        sb.add_task(f"Task {i}")
    displayed = sb.get_display_tasks(max_visible=3)
    assert len(displayed) <= 3
