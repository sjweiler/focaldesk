use smithay::utils::Logical;
use smithay::utils::Physical;
use smithay::utils::Rectangle;
use smithay::utils::Size;

/// Defaults aligned with `focaldesk_ui::chrome::ChromeMetrics::default` for nested (winit) mode.
pub const NESTED_DEFAULT_TOPBAR_H: i32 = 64;
pub const NESTED_DEFAULT_SIDEBAR_W: i32 = 76;
pub const DEFAULT_SIDEBAR_SLOT_COUNT: usize = 12;
/// Number of built-in status indicators. Runtime/custom collections override
/// this through `ChromeLayoutConfig::status_item_count` and are only limited by
/// the available output width.
pub const DEFAULT_TOPBAR_STATUS_COUNT: usize = 10;
pub const SIDEBAR_CORNER_RADIUS: f32 = 16.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChromeLayoutConfig {
    pub status_item_count: usize,
    pub sidebar_item_count: usize,
}

impl Default for ChromeLayoutConfig {
    fn default() -> Self {
        Self {
            status_item_count: DEFAULT_TOPBAR_STATUS_COUNT,
            sidebar_item_count: DEFAULT_SIDEBAR_SLOT_COUNT,
        }
    }
}

/// True when `(lx, ly)` (output-local **logical** coords) is in the top bar host-drag strip
/// but not on status/clock wells.
pub fn chrome_host_drag_hit(layout: &ChromeLayout, lx: i32, ly: i32) -> bool {
    if !layout.topbar.outer.contains((lx, ly)) {
        return false;
    }
    for well in &layout.topbar.status_wells {
        if well.contains((lx, ly)) {
            return false;
        }
    }
    if layout.topbar.clock_well.contains((lx, ly)) {
        return false;
    }
    true
}

/// Index of the sidebar slot under output-local **logical** `(lx, ly)`, using each slot's outer module rect.
pub fn sidebar_slot_index_at(layout: &ChromeLayout, lx: i32, ly: i32) -> Option<usize> {
    layout
        .sidebar
        .slots
        .iter()
        .enumerate()
        .find(|(_, slot)| slot.outer.contains((lx, ly)))
        .map(|(i, _)| i)
}

/// Index of the topbar status well under output-local **logical** `(lx, ly)`.
pub fn topbar_status_well_index_at(layout: &ChromeLayout, lx: i32, ly: i32) -> Option<usize> {
    layout
        .topbar
        .status_wells
        .iter()
        .enumerate()
        .find(|(_, well)| well.contains((lx, ly)))
        .map(|(i, _)| i)
}

/// Scale every chrome rectangle from logical layout space into framebuffer (physical) space.
/// Used by the GPU path only; hit testing and UI element bounds stay in logical space.
pub fn scale_chrome_layout(layout: &ChromeLayout, scale: f64) -> ChromeLayout<Physical> {
    let sc = |r: Rectangle<i32, Logical>| -> Rectangle<i32, Physical> {
        let x = (r.loc.x as f64 * scale).round() as i32;
        let y = (r.loc.y as f64 * scale).round() as i32;
        let w = (r.size.w as f64 * scale).round() as i32;
        let h = (r.size.h as f64 * scale).round() as i32;
        Rectangle::from_loc_and_size((x, y), (w.max(1), h.max(1)))
    };
    let sc_opt = |o: Option<Rectangle<i32, Logical>>| o.map(&sc);
    let sc_vec = |v: &[Rectangle<i32, Logical>]| v.iter().copied().map(&sc).collect();
    ChromeLayout {
        topbar: TopBarLayout {
            outer: sc(layout.topbar.outer),
            ai_button: sc(layout.topbar.ai_button),
            inner: sc(layout.topbar.inner),
            title: sc(layout.topbar.title),
            trim: sc(layout.topbar.trim),
            status_wells: sc_vec(&layout.topbar.status_wells),
            clock_well: sc(layout.topbar.clock_well),
            light: sc_opt(layout.topbar.light),
        },
        sidebar: SidebarLayout {
            outer: sc(layout.sidebar.outer),
            inner: sc(layout.sidebar.inner),
            slots: layout
                .sidebar
                .slots
                .iter()
                .map(|slot| SidebarSlotLayout {
                    outer: sc(slot.outer),
                    inner: sc(slot.inner),
                    icon_well: sc(slot.icon_well),
                })
                .collect(),
            light: sc_opt(layout.sidebar.light),
            caps: sc_vec(&layout.sidebar.caps),
        },
        work_area: WorkAreaLayout {
            outer: sc(layout.work_area.outer),
            inner_frame: sc(layout.work_area.inner_frame),
            recess: sc(layout.work_area.recess),
            glass: sc(layout.work_area.glass),
            trim: sc_opt(layout.work_area.trim),
        },
        decoration: ChromeDecorationLayout {
            corner_caps: sc_vec(&layout.decoration.corner_caps),
            corner_joint_caps: sc_vec(&layout.decoration.corner_joint_caps),
        },
    }
}

/// Chrome regions in **logical** output-local coordinates (default).
pub type ChromeLayoutLogical = ChromeLayout<Logical>;

/// Same geometry as [`ChromeLayoutLogical`], in physical pixels for GL drawing.
pub type ChromeLayoutPhysical = ChromeLayout<Physical>;

#[derive(Debug, Clone)]
pub struct ChromeLayout<Kind = Logical> {
    pub topbar: TopBarLayout<Kind>,
    pub sidebar: SidebarLayout<Kind>,
    pub work_area: WorkAreaLayout<Kind>,
    pub decoration: ChromeDecorationLayout<Kind>,
}

#[derive(Debug, Clone)]
pub struct TopBarLayout<Kind = Logical> {
    pub outer: Rectangle<i32, Kind>,
    pub ai_button: Rectangle<i32, Kind>,
    pub inner: Rectangle<i32, Kind>,
    pub title: Rectangle<i32, Kind>,
    pub trim: Rectangle<i32, Kind>,
    pub status_wells: Vec<Rectangle<i32, Kind>>,
    pub clock_well: Rectangle<i32, Kind>,
    pub light: Option<Rectangle<i32, Kind>>,
}

#[derive(Debug, Clone)]
pub struct SidebarLayout<Kind = Logical> {
    pub outer: Rectangle<i32, Kind>,
    pub inner: Rectangle<i32, Kind>,
    pub slots: Vec<SidebarSlotLayout<Kind>>,
    pub light: Option<Rectangle<i32, Kind>>,
    pub caps: Vec<Rectangle<i32, Kind>>,
}

#[derive(Debug, Clone)]
pub struct SidebarSlotLayout<Kind = Logical> {
    pub outer: Rectangle<i32, Kind>,
    pub inner: Rectangle<i32, Kind>,
    pub icon_well: Rectangle<i32, Kind>,
}

#[derive(Debug, Clone)]
pub struct WorkAreaLayout<Kind = Logical> {
    pub outer: Rectangle<i32, Kind>,
    pub inner_frame: Rectangle<i32, Kind>,
    pub recess: Rectangle<i32, Kind>,
    pub glass: Rectangle<i32, Kind>,
    pub trim: Option<Rectangle<i32, Kind>>,
}

#[derive(Debug, Clone)]
pub struct ChromeDecorationLayout<Kind = Logical> {
    pub corner_caps: Vec<Rectangle<i32, Kind>>,
    pub corner_joint_caps: Vec<Rectangle<i32, Kind>>,
}

impl ChromeLayout<Logical> {
    /// Clone logical geometry without requiring Smithay's marker type to implement `Clone`.
    pub fn clone_logical(&self) -> Self {
        Self {
            topbar: TopBarLayout {
                outer: self.topbar.outer,
                ai_button: self.topbar.ai_button,
                inner: self.topbar.inner,
                title: self.topbar.title,
                trim: self.topbar.trim,
                status_wells: self.topbar.status_wells.clone(),
                clock_well: self.topbar.clock_well,
                light: self.topbar.light,
            },
            sidebar: SidebarLayout {
                outer: self.sidebar.outer,
                inner: self.sidebar.inner,
                slots: self
                    .sidebar
                    .slots
                    .iter()
                    .map(|slot| SidebarSlotLayout {
                        outer: slot.outer,
                        inner: slot.inner,
                        icon_well: slot.icon_well,
                    })
                    .collect(),
                light: self.sidebar.light,
                caps: self.sidebar.caps.clone(),
            },
            work_area: WorkAreaLayout {
                outer: self.work_area.outer,
                inner_frame: self.work_area.inner_frame,
                recess: self.work_area.recess,
                glass: self.work_area.glass,
                trim: self.work_area.trim,
            },
            decoration: ChromeDecorationLayout {
                corner_caps: self.decoration.corner_caps.clone(),
                corner_joint_caps: self.decoration.corner_joint_caps.clone(),
            },
        }
    }

    /// Compare logical geometry without requiring equality on Smithay's marker type.
    pub fn same_geometry(&self, other: &Self) -> bool {
        fn rect_key(rect: Rectangle<i32, Logical>) -> [i32; 4] {
            [rect.loc.x, rect.loc.y, rect.size.w, rect.size.h]
        }
        fn rects_equal(
            left: &[Rectangle<i32, Logical>],
            right: &[Rectangle<i32, Logical>],
        ) -> bool {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| rect_key(*left) == rect_key(*right))
        }
        fn optional_rects_equal(
            left: Option<Rectangle<i32, Logical>>,
            right: Option<Rectangle<i32, Logical>>,
        ) -> bool {
            left.map(rect_key) == right.map(rect_key)
        }

        rect_key(self.topbar.outer) == rect_key(other.topbar.outer)
            && rect_key(self.topbar.ai_button) == rect_key(other.topbar.ai_button)
            && rect_key(self.topbar.inner) == rect_key(other.topbar.inner)
            && rect_key(self.topbar.title) == rect_key(other.topbar.title)
            && rect_key(self.topbar.trim) == rect_key(other.topbar.trim)
            && rects_equal(&self.topbar.status_wells, &other.topbar.status_wells)
            && rect_key(self.topbar.clock_well) == rect_key(other.topbar.clock_well)
            && optional_rects_equal(self.topbar.light, other.topbar.light)
            && rect_key(self.sidebar.outer) == rect_key(other.sidebar.outer)
            && rect_key(self.sidebar.inner) == rect_key(other.sidebar.inner)
            && self.sidebar.slots.len() == other.sidebar.slots.len()
            && self
                .sidebar
                .slots
                .iter()
                .zip(&other.sidebar.slots)
                .all(|(left, right)| {
                    rect_key(left.outer) == rect_key(right.outer)
                        && rect_key(left.inner) == rect_key(right.inner)
                        && rect_key(left.icon_well) == rect_key(right.icon_well)
                })
            && optional_rects_equal(self.sidebar.light, other.sidebar.light)
            && rects_equal(&self.sidebar.caps, &other.sidebar.caps)
            && rect_key(self.work_area.outer) == rect_key(other.work_area.outer)
            && rect_key(self.work_area.inner_frame) == rect_key(other.work_area.inner_frame)
            && rect_key(self.work_area.recess) == rect_key(other.work_area.recess)
            && rect_key(self.work_area.glass) == rect_key(other.work_area.glass)
            && optional_rects_equal(self.work_area.trim, other.work_area.trim)
            && rects_equal(&self.decoration.corner_caps, &other.decoration.corner_caps)
            && rects_equal(
                &self.decoration.corner_joint_caps,
                &other.decoration.corner_joint_caps,
            )
    }
}

fn inset_rect<Kind>(rect: Rectangle<i32, Kind>, inset: i32) -> Rectangle<i32, Kind> {
    let x = rect.loc.x + inset;
    let y = rect.loc.y + inset;
    let w = (rect.size.w - inset * 2).max(1);
    let h = (rect.size.h - inset * 2).max(1);

    Rectangle::from_loc_and_size((x, y), (w, h))
}

#[allow(clippy::type_complexity)]
fn build_status_cluster(
    topbar_inner: Rectangle<i32, Logical>,
    right_pad: i32,
    inter_gap: i32,
    clock_gap: i32,
    num_status: usize,
) -> (
    Rectangle<i32, Logical>,      // cluster
    Vec<Rectangle<i32, Logical>>, // status wells
    Rectangle<i32, Logical>,      // clock well
) {
    let h = topbar_inner.size.h;

    // vertical padding inside the bar
    let pad_y = (h / 6).max(4);
    let well_h = (h - pad_y * 2).max(18);

    // notifier wells
    let small_w = well_h;

    // clock gets more room because text
    let clock_w = ((well_h as f32) * 3.2) as i32;

    let total_status_w = if num_status > 0 {
        (num_status as i32 * small_w) + ((num_status as i32 - 1).max(0) * inter_gap)
    } else {
        0
    };

    let cluster_w = total_status_w + if num_status > 0 { clock_gap } else { 0 } + clock_w;
    let cluster_h = well_h;

    let cluster_x = topbar_inner.loc.x + topbar_inner.size.w - right_pad - cluster_w;
    let cluster_y = topbar_inner.loc.y + (topbar_inner.size.h - cluster_h) / 2;

    let cluster = Rectangle::from_loc_and_size((cluster_x, cluster_y), (cluster_w, cluster_h));

    let mut wells = Vec::with_capacity(num_status);
    let mut x = cluster.loc.x;

    for _ in 0..num_status {
        let r = Rectangle::from_loc_and_size((x, cluster.loc.y), (small_w, cluster_h));
        wells.push(r);
        x += small_w + inter_gap;
    }

    if num_status > 0 {
        x += clock_gap - inter_gap;
    }

    let clock = Rectangle::from_loc_and_size((x, cluster.loc.y), (clock_w, cluster_h));

    (cluster, wells, clock)
}

fn status_items_that_fit(topbar_inner: Rectangle<i32, Logical>, requested: usize) -> usize {
    let pad_y = (topbar_inner.size.h / 6).max(4);
    let well = (topbar_inner.size.h - pad_y * 2).max(18);
    let clock = ((well as f32) * 3.2) as i32;
    // Preserve the flow field, a small title region, cluster padding, and gaps.
    let available = (topbar_inner.size.w - 96 - 24 - clock - 54).max(0);
    let per_item = well + 6;
    let fit = (available / per_item).max(0) as usize;
    if requested == 0 {
        0
    } else {
        requested.min(fit.max(1))
    }
}

pub fn build_chrome_layout(
    output_size: Size<i32, Logical>,
    top_h: i32,
    left_w: i32,
) -> ChromeLayout {
    build_chrome_layout_with_config(output_size, top_h, left_w, ChromeLayoutConfig::default())
}

pub fn build_chrome_layout_with_config(
    output_size: Size<i32, Logical>,
    top_h: i32,
    left_w: i32,
    config: ChromeLayoutConfig,
) -> ChromeLayout {
    let w = output_size.w.max(1);
    let h = output_size.h.max(1);

    let top_h = top_h.max(40);
    let left_w = left_w.max(48);

    // -------------------------------------------------------------------------
    // 1. OUTER REGIONS
    // -------------------------------------------------------------------------

    let topbar_outer = Rectangle::from_loc_and_size((0, 0), (w, top_h));

    let work_outer =
        Rectangle::from_loc_and_size((left_w, top_h), ((w - left_w).max(1), (h - top_h).max(1)));

    // -------------------------------------------------------------------------
    // 2. WORK AREA STACK
    // -------------------------------------------------------------------------

    let work_inner_frame = inset_rect(work_outer, 2);
    let work_recess = inset_rect(work_inner_frame, 4);
    // Keep the glass overlay aligned with the actual work recess so the
    // client area and the visible glass backdrop describe the same region.
    let glass_rect = work_recess;

    // Keep decorative trim in the frame above the client recess. If it overlaps
    // the recess, ordinary client buffer damage is misclassified as shell damage
    // and a terminal repaint can redraw this entire strip.
    let work_trim = Some(Rectangle::from_loc_and_size(
        (work_inner_frame.loc.x + 6, work_inner_frame.loc.y),
        ((work_inner_frame.size.w - 12).max(1), 4),
    ));

    // -------------------------------------------------------------------------
    // 3. TOP BAR STACK
    // -------------------------------------------------------------------------

    // Keep the inner top bar only over the main chrome span, not over the sidebar.
    let topbar_inner = Rectangle::from_loc_and_size(
        (left_w + 4, 3),
        ((w - left_w - 8).max(1), (top_h - 6).max(1)),
    );

    let topbar_trim = Rectangle::from_loc_and_size((left_w + 6, 4), ((w - left_w - 12).max(1), 6));

    let topbar_light = Some(Rectangle::from_loc_and_size(
        (
            topbar_inner.loc.x + 6,
            topbar_inner.loc.y + topbar_inner.size.h - 5,
        ),
        ((topbar_inner.size.w - 12).max(1), 3),
    ));

    // Right-side cluster inside topbar_inner
    let status_count = status_items_that_fit(topbar_inner, config.status_item_count);
    let (status_cluster, status_wells, clock_well) =
        build_status_cluster(topbar_inner, 10, 6, 8, status_count);

    let control_left = topbar_inner.loc.x + 6;
    let ai_button_w = status_cluster.size.h;
    let ai_button = Rectangle::from_loc_and_size(
        (control_left, status_cluster.loc.y),
        (ai_button_w, status_cluster.size.h),
    );
    // Title gets the remaining space to the left of the cluster
    let title_left = ai_button.loc.x + ai_button.size.w + 12;
    let title_right = (status_cluster.loc.x - 10).max(title_left + 24);

    let title_rect = Rectangle::from_loc_and_size(
        (title_left, topbar_inner.loc.y + 4),
        (
            (title_right - title_left).max(24),
            (topbar_inner.size.h - 8).max(1),
        ),
    );

    // -------------------------------------------------------------------------
    // 4. SIDEBAR STACK
    // -------------------------------------------------------------------------

    let module_h = 48;
    let module_gap = 8;
    let module_margin_x = 8;
    let module_margin_top = 10;
    let module_margin_bottom = 10;

    let module_w = (left_w - module_margin_x * 2).max(16);
    let available_h = (h - top_h - module_margin_top - module_margin_bottom).max(0);
    let max_slots_that_fit = ((available_h + module_gap) / (module_h + module_gap)).max(0) as usize;
    let slot_count = config.sidebar_item_count.min(max_slots_that_fit);
    let modules_h = if slot_count == 0 {
        0
    } else {
        slot_count as i32 * module_h + (slot_count as i32 - 1) * module_gap
    };
    let sidebar_h = (module_margin_top + modules_h + module_margin_bottom)
        .min((h - top_h).max(1))
        .max(1);
    let sidebar_y = top_h + ((h - top_h - sidebar_h).max(0) / 2);
    let sidebar_outer = Rectangle::from_loc_and_size((0, sidebar_y), (left_w, sidebar_h));
    let sidebar_inner = inset_rect(sidebar_outer, 4);

    let mut slots = Vec::with_capacity(slot_count);
    let mut y = sidebar_outer.loc.y + module_margin_top;

    for _ in 0..slot_count {
        let outer = Rectangle::from_loc_and_size((module_margin_x, y), (module_w, module_h));
        let inner = inset_rect(outer, 2);
        let well = inset_rect(inner, 3);

        slots.push(SidebarSlotLayout {
            outer,
            inner,
            icon_well: well,
        });
        y += module_h + module_gap;
    }

    // Keeping your existing field name for now, even though this is really more
    // like a sidebar accent rail than a per-slot light.
    let sidebar_light_rect = Some(Rectangle::from_loc_and_size(
        (6, sidebar_outer.loc.y + 12),
        (3, (sidebar_outer.size.h - 24).max(1)),
    ));

    // The compact rail's rounded shell replaces the old square end-caps.
    let sidebar_caps = Vec::new();

    // -------------------------------------------------------------------------
    // 5. DECORATIVE CAPS / JOINTS
    // -------------------------------------------------------------------------

    let cap = 6;
    let corner_caps = vec![Rectangle::from_loc_and_size((w - cap, 0), (cap, cap))];

    // The dock is now detached from the top bar, so it has no square joint.
    let corner_joint_caps = Vec::new();

    // -------------------------------------------------------------------------
    // 6. FINAL STRUCT
    // -------------------------------------------------------------------------

    ChromeLayout {
        // Top bar
        topbar: TopBarLayout {
            outer: topbar_outer,
            ai_button,
            inner: topbar_inner,
            title: title_rect,
            trim: topbar_trim,
            status_wells,
            clock_well,
            light: topbar_light,
        },
        // Sidebar
        sidebar: SidebarLayout {
            outer: sidebar_outer,
            inner: sidebar_inner,
            slots,
            light: sidebar_light_rect,
            caps: sidebar_caps,
        },
        // Work area
        work_area: WorkAreaLayout {
            outer: work_outer,
            inner_frame: work_inner_frame,
            recess: work_recess,
            glass: glass_rect,
            trim: work_trim,
        },
        // Decorative joints
        decoration: ChromeDecorationLayout {
            corner_caps,
            corner_joint_caps,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_area_glass_matches_work_recess() {
        let layout = build_chrome_layout(Size::from((1920, 1080)), 64, 76);
        assert_eq!(layout.work_area.glass, layout.work_area.recess);
    }

    #[test]
    fn logical_layout_copy_preserves_all_geometry() {
        let layout = build_chrome_layout_with_config(
            Size::from((1920, 1080)),
            64,
            76,
            ChromeLayoutConfig {
                status_item_count: 8,
                sidebar_item_count: 6,
            },
        );
        let copy = layout.clone_logical();

        assert!(layout.same_geometry(&copy));
    }

    #[test]
    fn logical_layout_comparison_detects_changes() {
        let layout = build_chrome_layout(Size::from((1920, 1080)), 64, 76);
        let changed = build_chrome_layout(Size::from((2560, 1440)), 64, 76);

        assert!(!layout.same_geometry(&changed));
    }

    #[test]
    fn layout_capacity_follows_dynamic_item_counts() {
        let layout = build_chrome_layout_with_config(
            Size::from((1920, 1080)),
            64,
            76,
            ChromeLayoutConfig {
                status_item_count: 3,
                sidebar_item_count: 7,
            },
        );

        assert_eq!(layout.topbar.status_wells.len(), 3);
        assert_eq!(layout.sidebar.slots.len(), 7);
        assert_eq!(layout.sidebar.outer.size.h, 404);
        assert_eq!(layout.sidebar.outer.loc.y, 370);
        assert!(layout.sidebar.caps.is_empty());
        assert!(layout.decoration.corner_joint_caps.is_empty());
    }

    #[test]
    fn topbar_status_layout_is_not_capped_at_six_items() {
        let layout = build_chrome_layout_with_config(
            Size::from((2560, 1440)),
            64,
            76,
            ChromeLayoutConfig {
                status_item_count: 12,
                sidebar_item_count: 0,
            },
        );

        assert_eq!(layout.topbar.status_wells.len(), 12);
    }

    #[test]
    fn compact_sidebar_preserves_the_full_left_work_area_reservation() {
        let layout = build_chrome_layout_with_config(
            Size::from((1920, 1080)),
            64,
            76,
            ChromeLayoutConfig {
                status_item_count: 0,
                sidebar_item_count: 3,
            },
        );

        assert_eq!(layout.sidebar.outer.size.h, 180);
        assert_eq!(layout.sidebar.outer.loc.y, 482);
        assert_eq!(layout.work_area.outer.loc.x, 76);
        assert_eq!(layout.work_area.outer.size.w, 1844);
    }

    #[test]
    fn title_follows_ai_button_without_an_activity_slot() {
        let layout = build_chrome_layout(Size::from((1920, 1080)), 64, 76);

        assert_eq!(
            layout.topbar.title.loc.x,
            layout.topbar.ai_button.loc.x + layout.topbar.ai_button.size.w + 12
        );
    }

    #[test]
    fn work_trim_does_not_overlap_client_recess() {
        let layout = build_chrome_layout(Size::from((1920, 1080)), 64, 76);
        let trim = layout.work_area.trim.expect("default layout has work trim");

        assert!(!trim.overlaps(layout.work_area.recess));
    }
}
