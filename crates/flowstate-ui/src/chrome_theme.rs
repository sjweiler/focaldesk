use flowstate_themes::ChromeTheme as FlowChromeTheme;

pub fn chrome_theme_from_flow_theme(chrome: &FlowChromeTheme) -> ChromeTheme {
    let mut legacy = default_chrome_theme();
    legacy.frame_outer.face_color = chrome.bg_color;
    legacy.panel_inner.face_color = chrome.panel_color;
    legacy.trim.face_color = chrome.trim_color;
    legacy.light.glow_color = chrome.accent_color;
    legacy.light.core_color = chrome.accent_color;
    legacy.button.glow_color = chrome.accent_color;
    legacy.glass.tint = chrome.glass_tint;
    legacy.top_bar.radius = chrome.corner_radius;
    legacy.top_bar.trim_color = chrome.trim_color;
    legacy
}

#[derive(Debug, Clone, Copy)]
pub struct BevelStyle {
    pub bevel: f32,
    pub softness: f32,
    pub glow_width: f32,
    pub glow_alpha: f32,
    pub inner_shadow: f32,
    pub face_color: [f32; 4],
    pub light_color: [f32; 4],
    pub shadow_color: [f32; 4],
    pub glow_color: [f32; 4],
}

#[derive(Debug, Clone, Copy)]
pub struct LightChannelStyle {
    pub slot_inset: f32,
    pub core_inset: f32,
    pub glow_radius: f32,
    pub softness: f32,
    pub housing_color: [f32; 4],
    pub glow_color: [f32; 4],
    pub core_color: [f32; 4],
}

#[derive(Debug, Clone, Copy)]
pub struct GlassStyle {
    pub opacity: f32,
    pub edge_width: f32,
    pub edge_brightness: f32,
    pub highlight_strength: f32,

    pub tint: [f32; 4],
    pub edge_color: [f32; 4],
}

#[derive(Debug, Clone, Copy)]
pub struct LineStyle {
    pub color: [f32; 4],
    pub thickness: i32,
    pub alpha: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct GlowStyle {
    pub color: [f32; 4],
    pub alpha: f32,
    pub inset: i32,
}

#[derive(Debug, Clone, Copy)]
pub struct ButtonStyle {
    pub bevel: f32,
    pub softness: f32,
    pub inner_shadow: f32,

    pub glow_strength: f32,
    pub glow_radius: f32,

    pub face_color: [f32; 4],
    pub shadow_color: [f32; 4],
    pub glow_color: [f32; 4],
}

#[derive(Clone, Copy, Debug)]
pub struct TopBarStyle {
    pub radius: f32,
    pub softness: f32,
    pub bevel: f32,
    pub highlight_strength: f32,
    pub shadow_strength: f32,
    pub trim_height: f32,
    pub trim_brightness: f32,
    pub face_color: [f32; 4],
    pub edge_color: [f32; 4],
    pub trim_color: [f32; 4],
}

#[derive(Debug, Clone, Copy)]
pub struct ChromeTheme {  
    // frame
    pub frame_outer: BevelStyle,  // frame outer    
    pub frame_inner: BevelStyle,  // frame inner
    
    // Surface layers
    pub panel_base: BevelStyle,  // panael base
    pub panel_inner: BevelStyle, // panel recess
    
    
    
    // functional areas
    pub sidebar: BevelStyle,
    pub module: BevelStyle,
    pub module_inner: BevelStyle,
    pub icon_well: BevelStyle,    
    pub icon_well_active: BevelStyle,
    
    // decorative / trim
    pub trim: BevelStyle,
    pub corner_cap: BevelStyle,
    
    // Effects
    pub light: LightChannelStyle,    
    pub glass: GlassStyle,
    
    pub line_highlight: LineStyle,
    pub line_groove: LineStyle,

    pub glow_active: GlowStyle,
    
    pub button: ButtonStyle,
    
    pub top_bar: TopBarStyle,
}

pub fn default_chrome_theme() -> ChromeTheme {
    ChromeTheme {
        frame_outer: BevelStyle {
            bevel: 4.0,
            softness: 1.15,
            glow_width: 0.0,
            glow_alpha: 0.0,
            face_color: [0.030, 0.050, 0.090, 1.0],
            inner_shadow: 3.5,
            light_color:  [0.165, 0.215, 0.305, 1.0],
            shadow_color: [0.006, 0.010, 0.018, 1.0],
            glow_color:   [0.0, 0.0, 0.0, 1.0],
        },

        frame_inner: BevelStyle {
            bevel: 3.0,
            softness: 1.2,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 2.2,
            face_color: [0.050, 0.075, 0.120, 1.0],
            light_color:  [0.185, 0.235, 0.325, 1.0],
            shadow_color: [0.010, 0.016, 0.026, 1.0],
            glow_color:   [0.0, 0.0, 0.0, 1.0],
        },

        panel_base: BevelStyle {
            bevel: 2.5,
            softness: 1.25,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 1.8,
            face_color: [0.060, 0.085, 0.135, 1.0],   // was too bright
            light_color:  [0.205, 0.255, 0.345, 1.0],
            shadow_color: [0.014, 0.020, 0.032, 1.0],
            glow_color:   [0.0, 0.0, 0.0, 1.0],
        },

        panel_inner: BevelStyle {
            bevel: 2.5,
            softness: 1.35,
            glow_width: 0.0,
            glow_alpha: 0.0,
            face_color:   [0.025, 0.045, 0.080, 1.0],
            inner_shadow: 4.8,   // increase
            light_color:  [0.105, 0.145, 0.220, 1.0],
            shadow_color: [0.004, 0.008, 0.015, 1.0],
            glow_color:   [0.0, 0.0, 0.0, 1.0],
        },

        trim: BevelStyle {
            bevel: 1.4,
            softness: 0.95,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 1.2,
            face_color:  [0.075, 0.105, 0.160, 1.0],
            light_color:  [0.235, 0.290, 0.380, 1.0],
            shadow_color: [0.020, 0.028, 0.040, 1.0],
            glow_color:   [0.0, 0.0, 0.0, 1.0],
        },

        sidebar: BevelStyle {
            bevel: 2.8,
            softness: 1.2,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 2.4,
            face_color:   [0.050, 0.073, 0.118, 1.0],
            light_color:  [0.155, 0.205, 0.290, 1.0],
            shadow_color: [0.008, 0.013, 0.022, 1.0],
            glow_color:   [0.0, 0.0, 0.0, 1.0],
        },
        
        module: BevelStyle {
            bevel: 2.4,
            softness: 1.1,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 1.8,
            face_color:   [0.070, 0.098, 0.150, 1.0],
            light_color:  [0.200, 0.250, 0.335, 1.0],
            shadow_color: [0.012, 0.018, 0.028, 1.0],
            glow_color:   [0.0, 0.0, 0.0, 1.0],
        },

        module_inner: BevelStyle {
            bevel: 2.0,
            softness: 1.15,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 3.0,
            face_color:   [0.040, 0.060, 0.102, 1.0],
            light_color:  [0.105, 0.145, 0.215, 1.0],
            shadow_color: [0.004, 0.008, 0.014, 1.0],
            glow_color:   [0.0, 0.0, 0.0, 1.0],
        },

        icon_well: BevelStyle {
            bevel: 1.4,
            softness: 0.8,
            glow_width: 3.0,
            glow_alpha: 0.015,
            inner_shadow: 5.5,
            face_color: [0.015, 0.022, 0.040, 1.0],
            light_color: [0.08, 0.11, 0.17, 1.0],
            shadow_color: [0.001, 0.002, 0.005, 1.0],
            glow_color: [0.03, 0.06, 0.12, 1.0],
        },

        icon_well_active: BevelStyle {
            bevel: 1.5,
            softness: 1.0,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 1.5,
            face_color: [0.03, 0.06, 0.11, 1.0],
            light_color: [0.16, 0.24, 0.38, 1.0],
            shadow_color: [0.00, 0.02, 0.05, 1.0],
            glow_color: [0.0, 0.0, 0.0, 1.0],
        },

        corner_cap: BevelStyle {
            bevel: 2.0,
            softness: 1.05,
            glow_width: 0.0,
            glow_alpha: 0.0,
            inner_shadow: 1.8,
            face_color:   [0.055, 0.078, 0.120, 1.0],
            light_color:  [0.170, 0.220, 0.305, 1.0],
            shadow_color: [0.008, 0.013, 0.022, 1.0],
            glow_color:   [0.0, 0.0, 0.0, 1.0],
        },

        light: LightChannelStyle {
            slot_inset: 1.0,
            core_inset: 3.0,
            glow_radius: 8.0,
            softness: 2.0,
            housing_color: [0.03, 0.05, 0.08, 1.0],
            glow_color: [0.18, 0.30, 0.55, 1.0],
            core_color: [0.10, 0.18, 0.34, 1.0],
        },
        
        glass: GlassStyle {
            opacity: 0.08,              // down from 0.90+
            edge_width: 12.0,           // tighter
            edge_brightness: 0.75,      // WAS TOO HIGH
            highlight_strength: 0.10,   // cut this a lot
            tint: [0.035, 0.085, 0.200, 1.0],   // darker tint
            edge_color: [0.30, 0.55, 0.95, 0.14],
        },
        
        line_highlight: LineStyle {
            color: [0.55, 0.75, 1.00, 1.0],
            thickness: 1,
            alpha: 0.10,
        },

        line_groove: LineStyle {
            color: [0.0, 0.0, 0.0, 1.0],
            thickness: 1,
            alpha: 0.28,
        },

        glow_active: GlowStyle {
            color: [0.35, 0.65, 1.00, 1.0],
            alpha: 0.08,
            inset: 0,
        },
        
        button: ButtonStyle {
            bevel: 3.0,
            softness: 1.5,
            inner_shadow: 0.7,

            glow_strength: 0.12,
            glow_radius: 0.55,

            face_color: [0.08, 0.08, 0.09, 1.0],
            shadow_color: [0.0, 0.0, 0.0, 1.0],

            // teal
            glow_color: [0.2, 0.9, 0.8, 1.0],
        },
        
        top_bar: TopBarStyle {
            radius: 10.0,
            softness: 1.8,
            bevel: 8.0,
            highlight_strength: 0.05,
            shadow_strength: 0.10,
            trim_height: 0.035,
            trim_brightness: 0.15,
            face_color: [0.025, 0.045, 0.085, 0.96],
            edge_color: [0.01, 0.015, 0.03, 1.0],
            trim_color: [0.72, 0.82, 0.95, 1.0],
        },
                
    }
}


/*
pub fn default_chrome_theme() -> ChromeTheme {
    ChromeTheme {
        frame_outer: BevelStyle {
            chamfer: 1.0,
            bevel: 4.0,
            softness: 0.8,
            light_dir: [0.7, -0.7],
            face_color: [0.15, 0.18, 0.24, 1.0],
            light_color: [0.45, 0.52, 0.62, 1.0],
            shadow_color: [0.02, 0.03, 0.05, 1.0],
        },
        frame_inner: BevelStyle {
            chamfer: 1.0,
            bevel: 4.0,
            softness: 2.0,
            light_dir: [0.7, -0.7],
            face_color: [0.07, 0.10, 0.15, 1.0],
            light_color: [0.8, 0.9, 1.0, 0.25],
shadow_color: [0.0, 0.0, 0.0, 0.5],
        },
        panel_base: BevelStyle {
            chamfer: 5.0,
            bevel: 3.0,
            softness: 2.0,
            light_dir: [0.7, -0.7],
            face_color: [0.05, 0.08, 0.12, 1.0],
            light_color: [0.14, 0.17, 0.24, 1.0],
            shadow_color: [0.00, 0.00, 0.01, 1.0],
        },
        panel_inner: BevelStyle {
            chamfer: 4.0,
            bevel: -2.0,
            softness: 2.0,
            light_dir: [0.7, -0.7],
            face_color: [0.03, 0.05, 0.09, 1.0],
            light_color: [0.10, 0.13, 0.19, 1.0],
            shadow_color: [0.00, 0.00, 0.00, 1.0],
        },
        trim: BevelStyle {
            chamfer: 3.0,
            bevel: 2.0,
            softness: 1.0,
            light_dir: [0.7, -0.7],
            face_color: [0.09, 0.11, 0.16, 1.0],
            light_color: [0.25, 0.30, 0.40, 1.0],
            shadow_color: [0.01, 0.02, 0.03, 1.0],
        },
        sidebar: BevelStyle {
            chamfer: 8.0,
            bevel: 2.5,
            softness: 0.6,
            light_dir: [0.7, -0.7],
            face_color:  [0.035, 0.050, 0.080, 1.0],
            light_color:  [0.22, 0.30, 0.40, 1.0],
            shadow_color: [0.010, 0.015, 0.025, 1.0],
        },
        module: BevelStyle {
            chamfer: 6.0,
            bevel: 2.0,
            softness: 0.6,
            light_dir: [0.7, -0.7],
            face_color:   [0.070, 0.095, 0.14, 1.0],
            light_color:  [0.22, 0.30, 0.40, 1.0],
            shadow_color: [0.010, 0.015, 0.025, 1.0],
        },
        module_inner: BevelStyle {
            chamfer: 4.0,
            bevel: 1.5,
            softness: 0.8,
            light_dir: [0.7, -0.7],
            face_color:   [0.050, 0.070, 0.11, 1.0],
            light_color:  [0.16, 0.22, 0.30, 1.0],
            shadow_color: [0.006, 0.010, 0.018, 1.0],        
        },
        icon_well: BevelStyle {
            chamfer: 4.0,
            bevel: 2.5,
            softness: 0.6,
            light_dir: [-0.4, 0.8], // different direction = visual separation
            face_color:   [0.035, 0.050, 0.085, 1.0],
            light_color:  [0.10, 0.14, 0.20, 1.0],
            shadow_color: [0.003, 0.005, 0.010, 1.0],
        },   
        corner_cap: BevelStyle {
            chamfer: 2.0,
            bevel: 2.0,
            softness: 1.0,
            light_dir: [0.7, -0.7],
            face_color: [0.12, 0.14, 0.19, 1.0],
            light_color: [0.35, 0.40, 0.50, 1.0],
            shadow_color: [0.02, 0.02, 0.03, 1.0],
        },
        light: LightChannelStyle {
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
*/
