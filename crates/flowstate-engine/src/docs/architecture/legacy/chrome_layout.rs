use smithay::utils::{Logical, Physical, Point, Rectangle, Size};

#[derive(Debug, Clone)]
pub struct ChromeLayout {
    pub top: Rectangle<i32, Physical>,
    pub left: Rectangle<i32, Physical>,
    pub content: Rectangle<i32, Physical>,
    pub content_outer: Rectangle<i32, Physical>,
    pub content_mid: Rectangle<i32, Physical>,
    pub content_inner: Rectangle<i32, Physical>,
    pub top_trim_outer: Rectangle<i32, Physical>,
    pub top_trim_inner: Rectangle<i32, Physical>,
    pub top_light: Rectangle<i32, Physical>,
    pub left_light: Rectangle<i32, Physical>,
    pub sidebar_modules: Vec<Rectangle<i32, Physical>>,
    pub top_slots: Vec<Rectangle<i32, Physical>>,
    pub corner_caps: Vec<Rectangle<i32, Physical>>,

    pub status_cluster: Rectangle<i32, Physical>,
    pub status_wells: Vec<Rectangle<i32, Physical>>,
    pub clock_well: Rectangle<i32, Physical>,
    pub title_rect: Rectangle<i32, Physical>,
}

#[inline]
pub fn inset_rect(
    r: Rectangle<i32, Physical>,
    px: i32,
) -> Rectangle<i32, Physical> {
    Rectangle::from_loc_and_size(
        (r.loc.x + px, r.loc.y + px),
        (
            (r.size.w - px * 2).max(1),
            (r.size.h - px * 2).max(1),
        ),
    )
}

pub fn well_icon_rect(well: Rectangle<i32, Physical>) -> Rectangle<i32, Physical> {
    inset_rect(well, (well.size.h / 5).max(4))
}

pub fn clock_text_rect(well: Rectangle<i32, Physical>) -> Rectangle<i32, Physical> {
    Rectangle::from_loc_and_size(
        (well.loc.x + 8, well.loc.y + 5),
        ((well.size.w - 16).max(1), (well.size.h - 10).max(1)),
    )
}

fn build_status_cluster(
    topbar_inner: Rectangle<i32, Physical>,
    right_pad: i32,
    inter_gap: i32,
    clock_gap: i32,
    num_status: usize,
) -> (
    Rectangle<i32, Physical>,      // cluster
    Vec<Rectangle<i32, Physical>>, // status wells
    Rectangle<i32, Physical>,      // clock well
) {
    let h = topbar_inner.size.h;

    // vertical padding inside the bar
    let pad_y = (h / 6).max(4);
    let well_h = (h - pad_y * 2).max(18);

    // notifier wells
    let small_w = well_h;

    // clock gets more room because text
    let clock_w = ((well_h as f32) * 2.6) as i32;

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

pub fn build_chrome_layout(output_size: Size<i32, Physical>) -> ChromeLayout {
    let w = output_size.w;
    let h = output_size.h;

    let top_h = 40;
    let left_w = 56;

    let top = Rectangle::from_loc_and_size((0, 0), (w, top_h));
    let left = Rectangle::from_loc_and_size((0, top_h), (left_w, h - top_h));
    let content = Rectangle::from_loc_and_size((left_w, top_h), (w - left_w, h - top_h));

    let content_outer = inset_rect(content, 2);
    let content_mid = inset_rect(content_outer, 4);
    let content_inner = inset_rect(content_mid, 4);

    let top_trim_outer = Rectangle::from_loc_and_size(
        (left_w + 8, 6),
        (w - left_w - 16, 8),
    );
    let top_trim_inner = inset_rect(top_trim_outer, 1);

    let top_light = Rectangle::from_loc_and_size(
        (left_w + 10, top_h - 10),
        (w - left_w - 20, 3),
    );

    let left_light = Rectangle::from_loc_and_size(
        (6, top_h + 12),
        (3, h - top_h - 24),
    );

    let mut sidebar_modules = Vec::new();
    let module_h = 64;
    let mut y = top_h + 10;
    while y + module_h < h - 40 {
        sidebar_modules.push(Rectangle::from_loc_and_size(
            (8, y),
            (left_w - 16, module_h),
        ));
        y += module_h + 8;
    }

    //
    // top-right status cluster
    //
    let pad_y = 5;
    let well_h = (top_h - pad_y * 2).max(18);

    let status_gap = 6;
    let clock_gap = 12;
    let right_pad = 10;

    let num_status_wells = 3;
    let status_well_w = well_h;
    let clock_well_w = ((well_h as f32) * 2.6) as i32;

    let status_total_w =
        num_status_wells * status_well_w + (num_status_wells - 1) * status_gap;
    let cluster_w = status_total_w + clock_gap + clock_well_w;
    let cluster_h = well_h;

    let cluster_x = w - right_pad - cluster_w;
    let cluster_y = (top_h - cluster_h) / 2;

    let status_cluster = Rectangle::from_loc_and_size(
        (cluster_x, cluster_y),
        (cluster_w, cluster_h),
    );

    let mut status_wells = Vec::new();
    let mut sx = cluster_x;
    for _ in 0..num_status_wells {
        status_wells.push(Rectangle::from_loc_and_size(
            (sx, cluster_y),
            (status_well_w, cluster_h),
        ));
        sx += status_well_w + status_gap;
    }

    sx += clock_gap - status_gap;

    let clock_well = Rectangle::from_loc_and_size(
        (sx, cluster_y),
        (clock_well_w, cluster_h),
    );

    //
    // title / logo zone
    //
    let title_left = 10;
    let title_right = status_cluster.loc.x - 14;
    let title_rect = Rectangle::from_loc_and_size(
        (title_left, 0),
        ((title_right - title_left).max(40), top_h),
    );

    //
    // optional top slots inside title zone
    //
    let mut top_slots = Vec::new();
    let slot_h = 18;
    let slot_w = 72;
    let slot_y = (top_h - slot_h) / 2;
    let mut x = title_left + 8;
    while x + slot_w < title_right {
        top_slots.push(Rectangle::from_loc_and_size(
            (x, slot_y),
            (slot_w, slot_h),
        ));
        x += slot_w + 6;
    }

    let cap = 12;
    let corner_caps = vec![
        Rectangle::from_loc_and_size((0, 0), (cap, cap)),
        Rectangle::from_loc_and_size((w - cap, 0), (cap, cap)),
        Rectangle::from_loc_and_size((0, h - cap), (cap, cap)),
        Rectangle::from_loc_and_size((left_w - cap, h - cap), (cap, cap)),
    ];

    ChromeLayout {
        top,
        left,
        content,
        content_outer,
        content_mid,
        content_inner,
        top_trim_outer,
        top_trim_inner,
        top_light,
        left_light,
        sidebar_modules,
        top_slots,
        corner_caps,
        status_cluster,
        status_wells,
        clock_well,
        title_rect,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BevelStyle {
    pub radius: f32,
    pub bevel: f32,
    pub softness: f32,
    pub light_dir: [f32; 2],
    pub face_color: [f32; 4],
    pub light_color: [f32; 4],
    pub shadow_color: [f32; 4],
}

#[derive(Debug, Clone, Copy)]
pub struct LightStyle {
    pub slot_inset: f32,
    pub core_inset: f32,
    pub glow_radius: f32,
    pub softness: f32,
    pub housing_color: [f32; 4],
    pub glow_color: [f32; 4],
    pub core_color: [f32; 4],
}

#[derive(Debug, Clone, Copy)]
pub struct ChromeTheme {
    pub housing: BevelStyle,
    pub recess_outer: BevelStyle,
    pub recess_mid: BevelStyle,
    pub recess_inner: BevelStyle,
    pub top_trim: BevelStyle,
    pub module_panel: BevelStyle,
    pub corner_cap: BevelStyle,
    pub light: LightStyle,
}

pub fn default_chrome_theme() -> ChromeTheme {
    ChromeTheme {
        housing: BevelStyle {
            radius: 8.0,
            bevel: 6.0,
            softness: 2.0,
            light_dir: [0.7, -0.7],
            face_color: [0.15, 0.18, 0.24, 1.0],
            light_color: [0.45, 0.52, 0.62, 1.0],
            shadow_color: [0.02, 0.03, 0.05, 1.0],
        },
        recess_outer: BevelStyle {
            radius: 6.0,
            bevel: 4.0,
            softness: 2.0,
            light_dir: [0.7, -0.7],
            face_color: [0.07, 0.10, 0.15, 1.0],
            light_color: [0.18, 0.22, 0.30, 1.0],
            shadow_color: [0.01, 0.01, 0.02, 1.0],
        },
        recess_mid: BevelStyle {
            radius: 5.0,
            bevel: 3.0,
            softness: 2.0,
            light_dir: [0.7, -0.7],
            face_color: [0.05, 0.08, 0.12, 1.0],
            light_color: [0.14, 0.17, 0.24, 1.0],
            shadow_color: [0.00, 0.00, 0.01, 1.0],
        },
        recess_inner: BevelStyle {
            radius: 4.0,
            bevel: 2.0,
            softness: 2.0,
            light_dir: [0.7, -0.7],
            face_color: [0.03, 0.05, 0.09, 1.0],
            light_color: [0.10, 0.13, 0.19, 1.0],
            shadow_color: [0.00, 0.00, 0.00, 1.0],
        },
        top_trim: BevelStyle {
            radius: 3.0,
            bevel: 2.0,
            softness: 1.0,
            light_dir: [0.7, -0.7],
            face_color: [0.09, 0.11, 0.16, 1.0],
            light_color: [0.25, 0.30, 0.40, 1.0],
            shadow_color: [0.01, 0.02, 0.03, 1.0],
        },
        module_panel: BevelStyle {
            radius: 4.0,
            bevel: 3.0,
            softness: 1.5,
            light_dir: [0.7, -0.7],
            face_color: [0.10, 0.13, 0.18, 1.0],
            light_color: [0.30, 0.36, 0.45, 1.0],
            shadow_color: [0.02, 0.02, 0.03, 1.0],
        },
        corner_cap: BevelStyle {
            radius: 2.0,
            bevel: 2.0,
            softness: 1.0,
            light_dir: [0.7, -0.7],
            face_color: [0.12, 0.14, 0.19, 1.0],
            light_color: [0.35, 0.40, 0.50, 1.0],
            shadow_color: [0.02, 0.02, 0.03, 1.0],
        },
        light: LightStyle {
            slot_inset: 1.0,
            core_inset: 2.0,
            glow_radius: 6.0,
            softness: 2.0,
            housing_color: [0.02, 0.05, 0.10, 1.0],
            glow_color: [0.20, 0.55, 1.00, 1.0],
            core_color: [0.75, 0.90, 1.00, 1.0],
        },
    }
}
