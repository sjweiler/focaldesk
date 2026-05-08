use smithay::utils::{Point, Rectangle, Size};
use smithay::utils::Logical;

/// Layout configuration (tweakable, eventually from settings)
#[derive(Debug, Clone)]
pub struct LayoutConfig {
    pub top_bar_h: i32,
    pub side_bar_w: i32,

    /// Optional padding inside work area (nice for shadows / gaps)
    pub work_pad_l: i32,
    pub work_pad_t: i32,
    pub work_pad_r: i32,
    pub work_pad_b: i32,
    
    // pip focus
    pub pip_l: i32,
    pub pip_t: i32,
    pub pip_r: i32,
    pub pip_b: i32,
}

impl Default for LayoutConfig {
    fn default() -> Self {
        Self {
            top_bar_h: 36,
            side_bar_w: 64,
            work_pad_l: 0,
            work_pad_t: 0,
            work_pad_r: 0,
            work_pad_b: 0,
            pip_l: 8,
            pip_t: 8,
            pip_r: 8,
            pip_b: 8,
        }
    }
}

/// One computed layout snapshot for a given output size
#[derive(Debug, Clone,Default)]
pub struct LayoutSnapshot {
    pub output: Rectangle<i32, Logical>,
    pub top_bar: Rectangle<i32, Logical>,
    pub side_bar: Rectangle<i32, Logical>,
    pub work_area: Rectangle<i32, Logical>,
    pub pip_focus: Rectangle<i32, Logical>,
    
}

impl LayoutSnapshot {
    /// Where a maximized toplevel should be drawn (top-left origin)
    pub fn client_origin(&self) -> Point<i32, Logical> {
        self.work_area.loc
    }

    /// The size we should configure XDG toplevels to
    pub fn client_size(&self) -> Size<i32, Logical> {
        self.work_area.size
    }
        pub fn compute(
        output: Rectangle<i32, Logical>,
        top_bar_h: i32,
        side_bar_w: i32,
    ) -> Self {
        let (ox, oy) = (output.loc.x, output.loc.y);
        let (ow, oh) = (output.size.w, output.size.h);

        let top_bar = Rectangle::new((ox, oy).into(), (ow, top_bar_h).into());

        let side_bar = Rectangle::new(
            (ox, oy + top_bar_h).into(),
            (side_bar_w, (oh - top_bar_h).max(1)).into(),
        );

        let work_area = Rectangle::new(
            (ox + side_bar_w, oy + top_bar_h).into(),
            ((ow - side_bar_w).max(1), (oh - top_bar_h).max(1)).into(),
        );

        // temp PiP: just a small rect inside work_area
        let pip_focus = Rectangle::new(work_area.loc, (1, 1).into());

        Self {
            output,
            top_bar,
            side_bar,
            work_area,
            pip_focus,
        }
    }
}



/// Stateless pure layout engine.
/// (You can make it store config and expose `compute()`.)
#[derive(Debug, Clone)]
pub struct LayoutEngine {
    cfg: LayoutConfig,
}

impl LayoutEngine {
    pub fn new(cfg: LayoutConfig) -> Self {
        Self { cfg }
    }

    pub fn config(&self) -> &LayoutConfig {
        &self.cfg
    }

    pub fn config_mut(&mut self) -> &mut LayoutConfig {
        &mut self.cfg
    }

    /// Compute a full layout in LOGICAL coordinates.
    /// `out_w/out_h` are logical pixels (already divided by scale factor).
    pub fn compute(&self, out_w: i32, out_h: i32) -> LayoutSnapshot {
        let out_w = out_w.max(1);
        let out_h = out_h.max(1);

        let output = Rectangle::new((0, 0).into(), (out_w, out_h).into());

        // Bars
        let top_bar_h = self.cfg.top_bar_h.clamp(0, out_h);
        let side_bar_w = self.cfg.side_bar_w.clamp(0, out_w);

        let top_bar = Rectangle::new((0, 0).into(), (out_w, top_bar_h).into());
        let side_bar = Rectangle::new(
            (0, top_bar_h).into(),
            (side_bar_w, (out_h - top_bar_h).max(0)).into(),
        );

        // Work area
        let mut wx = side_bar_w;
        let mut wy = top_bar_h;
        let mut ww = (out_w - side_bar_w).max(1);
        let mut wh = (out_h - top_bar_h).max(1);

        // Optional padding inside work area
        wx += self.cfg.work_pad_l;
        wy += self.cfg.work_pad_t;
        ww = (ww - (self.cfg.work_pad_l + self.cfg.work_pad_r)).max(1);
        wh = (wh - (self.cfg.work_pad_t + self.cfg.work_pad_b)).max(1);

        let work_area = Rectangle::new((wx, wy).into(), (ww, wh).into());

        // Pip fous area
        let mut px = side_bar_w;
        let mut py = top_bar_h;
        let mut pw = (out_w - side_bar_w).max(1);
        let mut ph = (out_h - top_bar_h).max(1);
        
        let pip_focus = Rectangle::new((px, py).into(), (pw, ph).into());

        LayoutSnapshot {
            output,
            top_bar,
            side_bar,
            work_area,
            pip_focus,
        }
    }
}


