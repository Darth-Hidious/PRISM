//! What is drawn where, so the pointer can mean something.
//!
//! The renderer paints text and forgets the layout, so a click carried no
//! information: `handle_mouse` read the wheel and dropped everything else, and
//! the cursor position was never read anywhere in the crate. Hovering a term,
//! clicking a row, and selecting a span all need the same missing fact — which
//! thing is under this cell — so it is recorded once here rather than three
//! times.
//!
//! The map is rebuilt every frame. Scroll offset, terminal size and which
//! overlay is open all move things, so a map that outlived a frame would point
//! at where something used to be. Cheap to refill: a push per drawn region.

use ratatui::layout::Rect;

use crate::app::WorkspaceTab;

/// Something on screen that can be pointed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitTarget {
    /// A tab label in the workspace strip.
    WorkspaceTab(WorkspaceTab),
    /// One row of the active workspace tab, by its index in that tab's list.
    WorkspaceRow { tab: WorkspaceTab, index: usize },
    /// ONE rendered row of the transcript, carrying the text that was on it.
    ///
    /// The text is what was DRAWN, not a slice of the source: markdown
    /// transforms the message before it reaches the screen, so a rendered row
    /// is often not a substring of `ChatLine::text`. What the reader pointed
    /// at is what they saw, so that is what gets quoted back.
    TranscriptLine { message: usize, text: String },
    /// A message in the transcript, by its index in `App::messages`.
    TranscriptMessage { index: usize },
    /// The close control on the reference panel.
    RefPanelClose,
    /// A marked reference inside rendered text — the thing hovering resolves.
    /// Holds only the reference, never the payload: what it points at is
    /// fetched when the pointer arrives, not when the text was written.
    Reference { id: String },
}

/// Regions of the last drawn frame, newest last.
#[derive(Debug, Default)]
pub struct HitMap {
    regions: Vec<(Rect, HitTarget)>,
}

impl HitMap {
    /// Drop the previous frame's regions. Called once at the start of a draw.
    pub fn clear(&mut self) {
        self.regions.clear();
    }

    /// Record that `target` occupies `rect`.
    ///
    /// Empty rects are dropped rather than stored: a zero-width or zero-height
    /// region can never contain a cell, and keeping them would mean every
    /// lookup walks past regions that cannot match.
    pub fn push(&mut self, rect: Rect, target: HitTarget) {
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        self.regions.push((rect, target));
    }

    /// The target at a screen cell, or `None`.
    ///
    /// Searched newest-first because the renderer draws overlays last and they
    /// sit on top: a popup covering a transcript line must answer for those
    /// cells, or clicking a button would hit the text behind it.
    #[must_use]
    pub fn at(&self, column: u16, row: u16) -> Option<&HitTarget> {
        self.regions
            .iter()
            .rev()
            .find(|(rect, _)| {
                column >= rect.x
                    && column < rect.x + rect.width
                    && row >= rect.y
                    && row < rect.y + rect.height
            })
            .map(|(_, target)| target)
    }

    /// Number of recorded regions — for tests and diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.regions.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_with(regions: &[(Rect, HitTarget)]) -> HitMap {
        let mut map = HitMap::default();
        for (rect, target) in regions {
            map.push(*rect, target.clone());
        }
        map
    }

    #[test]
    fn a_cell_inside_a_region_finds_its_target() {
        let map = map_with(&[(
            Rect::new(4, 2, 10, 3),
            HitTarget::WorkspaceRow {
                tab: WorkspaceTab::Artifacts,
                index: 7,
            },
        )]);
        assert_eq!(
            map.at(4, 2),
            Some(&HitTarget::WorkspaceRow {
                tab: WorkspaceTab::Artifacts,
                index: 7
            }),
            "the top-left cell is inside the region"
        );
        assert_eq!(
            map.at(13, 4),
            Some(&HitTarget::WorkspaceRow {
                tab: WorkspaceTab::Artifacts,
                index: 7
            }),
            "the bottom-right cell is inside the region"
        );
    }

    /// A rect covers `[x, x+width)` — the far edge belongs to whatever is drawn
    /// next to it. Off by one here would make every row steal its neighbour's
    /// first column.
    #[test]
    fn the_far_edge_belongs_to_the_next_region() {
        let map = map_with(&[(
            Rect::new(0, 0, 5, 1),
            HitTarget::TranscriptMessage { index: 0 },
        )]);
        assert!(
            map.at(4, 0).is_some(),
            "x=4 is the last cell of a width-5 rect"
        );
        assert_eq!(map.at(5, 0), None, "x=5 is past the end");
        assert_eq!(map.at(0, 1), None, "y=1 is past a height-1 rect");
    }

    /// Overlays are drawn last and sit on top, so they must answer for the
    /// cells they cover. Searching in insertion order would return the
    /// transcript line hidden behind a popup.
    #[test]
    fn the_last_thing_drawn_wins_the_cell() {
        let map = map_with(&[
            (
                Rect::new(0, 0, 20, 10),
                HitTarget::TranscriptMessage { index: 3 },
            ),
            (
                Rect::new(5, 5, 4, 2),
                HitTarget::Reference {
                    id: "structure:cache://a3f9".to_string(),
                },
            ),
        ]);
        assert_eq!(
            map.at(6, 6),
            Some(&HitTarget::Reference {
                id: "structure:cache://a3f9".to_string()
            }),
            "the overlapping region drawn later owns the cell"
        );
        assert_eq!(
            map.at(1, 1),
            Some(&HitTarget::TranscriptMessage { index: 3 }),
            "cells the later region does not cover still resolve to the earlier one"
        );
    }

    #[test]
    fn an_empty_rect_is_never_recorded() {
        let mut map = HitMap::default();
        map.push(
            Rect::new(3, 3, 0, 5),
            HitTarget::WorkspaceTab(WorkspaceTab::Tools),
        );
        map.push(
            Rect::new(3, 3, 5, 0),
            HitTarget::WorkspaceTab(WorkspaceTab::Tools),
        );
        assert!(
            map.is_empty(),
            "a zero-sized region can never contain a cell"
        );
        assert_eq!(map.at(3, 3), None);
    }

    /// The map describes ONE frame. Scroll, resize and opening an overlay all
    /// move things, so a stale entry points at where something used to be.
    #[test]
    fn clearing_drops_the_previous_frames_regions() {
        let mut map = map_with(&[(
            Rect::new(0, 0, 10, 10),
            HitTarget::TranscriptMessage { index: 1 },
        )]);
        assert_eq!(map.len(), 1);
        map.clear();
        assert!(map.is_empty());
        assert_eq!(
            map.at(5, 5),
            None,
            "a cleared map must not answer from the last frame"
        );
    }
}
