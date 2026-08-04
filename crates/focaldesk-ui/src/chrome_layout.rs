use smithay::utils::Logical;
use smithay::utils::Physical;
use smithay::utils::Rectangle;
use smithay::utils::Size;

/// Defaults aligned with `focaldesk_ui::chrome::ChromeMetrics::default` for nested (winit) mode.
pub const NESTED_DEFAULT_TOPBAR_H: i32 = 64;
pub const NESTED_DEFAULT_SIDEBAR_W: i32 = 76;
pub const DEFAULT_SIDEBAR_SLOT_COUNT: usize = 12;
pub const DEFAULT_TOPBAR_STATUS_COUNT: usize = 6;

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
            flow_field: sc(layout.topbar.flow_field),
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
    pub flow_field: Rectangle<i32, Kind>,
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

    let sidebar_outer = Rectangle::from_loc_and_size((0, top_h), (left_w, (h - top_h).max(1)));

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

    let work_trim = Some(Rectangle::from_loc_and_size(
        (work_inner_frame.loc.x + 6, work_inner_frame.loc.y + 4),
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

    let flow_left = topbar_inner.loc.x + 6;
    let flow_gap = 10;
    let flow_available = (status_cluster.loc.x - flow_left - flow_gap).max(0);
    let flow_preferred = 136;
    let flow_min = 96;
    let flow_w = if flow_available <= flow_min {
        flow_available
    } else {
        flow_available.min(flow_preferred).max(flow_min)
    }
    .max(1);

    let flow_field = Rectangle::from_loc_and_size(
        (flow_left, status_cluster.loc.y),
        (flow_w, status_cluster.size.h),
    );

    // Title gets the remaining space to the left of the cluster
    let title_left = flow_field.loc.x + flow_field.size.w + 12;
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

    let mut slots = Vec::new();

    let sidebar_inner = inset_rect(sidebar_outer, 4);

    //let mut slot_outer_rects = Vec::new();
    // let mut slot_inner_rects = Vec::new();
    // let mut slot_icon_wells = Vec::new();

    let module_h = 48;
    let module_gap = 8;
    let module_margin_x = 8;
    let module_margin_top = 10;
    let module_margin_bottom = 24;

    let module_w = (left_w - module_margin_x * 2).max(16);
    let mut y = top_h + module_margin_top;

    let available_h = (h - top_h - module_margin_top - module_margin_bottom).max(0);
    let max_slots_that_fit = ((available_h + module_gap) / (module_h + module_gap)).max(0) as usize;
    let slot_count = config.sidebar_item_count.min(max_slots_that_fit);

    for _ in 0..slot_count {
        let outer = Rectangle::from_loc_and_size((module_margin_x, y), (module_w, module_h));
        let inner = inset_rect(outer, 2);
        let well = inset_rect(inner, 3);

        slots.push(SidebarSlotLayout {
            outer,
            inner,
            icon_well: well,
        });

        //slot_outer_rects.push(outer);
        //slot_inner_rects.push(inner);
        //slot_icon_wells.push(well);

        y += module_h + module_gap;
    }

    // Keeping your existing field name for now, even though this is really more
    // like a sidebar accent rail than a per-slot light.
    let sidebar_light_rect = Some(Rectangle::from_loc_and_size(
        (6, top_h + 12),
        (3, (h - top_h - 24).max(1)),
    ));

    // Top and bottom end-caps for the sidebar.
    let cap_h = 6;
    let sidebar_caps: Vec<Rectangle<i32, Logical>> = vec![
        Rectangle::from_loc_and_size(
            (sidebar_outer.loc.x, sidebar_outer.loc.y),
            (sidebar_outer.size.w, cap_h),
        ),
        Rectangle::from_loc_and_size(
            (
                sidebar_outer.loc.x,
                sidebar_outer.loc.y + sidebar_outer.size.h - cap_h,
            ),
            (sidebar_outer.size.w, cap_h),
        ),
    ];

    // -------------------------------------------------------------------------
    // 5. DECORATIVE CAPS / JOINTS
    // -------------------------------------------------------------------------

    let cap = 6;
    let corner_caps = vec![
        // top-right of top bar
        Rectangle::from_loc_and_size((w - cap, 0), (cap, cap)),
        // bottom-left of sidebar
        Rectangle::from_loc_and_size((0, h - cap), (cap, cap)),
        // bottom-right of sidebar
        Rectangle::from_loc_and_size((left_w - cap, h - cap), (cap, cap)),
    ];

    let corner_joint_caps = vec![
        // top-left joint where top bar meets sidebar
        Rectangle::from_loc_and_size((0, top_h.saturating_sub(2)), (8, 6)),
        Rectangle::from_loc_and_size((0, top_h.saturating_sub(2)), (6, 8)),
    ];

    // -------------------------------------------------------------------------
    // 6. FINAL STRUCT
    // -------------------------------------------------------------------------

    ChromeLayout {
        // Top bar
        topbar: TopBarLayout {
            outer: topbar_outer,
            flow_field,
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
    }

    #[test]
    fn flow_field_matches_status_and_clock_vertical_clearance() {
        let layout = build_chrome_layout(Size::from((1920, 1080)), 64, 76);
        let flow = layout.topbar.flow_field;
        let clock = layout.topbar.clock_well;

        assert_eq!(flow.loc.y, clock.loc.y);
        assert_eq!(flow.size.h, clock.size.h);
        assert!(
            layout
                .topbar
                .status_wells
                .iter()
                .all(|well| well.loc.y == flow.loc.y && well.size.h == flow.size.h)
        );
    }
}
