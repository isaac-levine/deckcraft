//! Rendered slides as GPU textures: the canvas slide and thumbnails, cached by the slide's and its
//! master's `Arc` identity (an unchanged slide keeps its `Arc`, so it never re-renders).
//!
//! The canvas texture covers the slide plus whatever of the slide's drawing reaches off it and is
//! in view (PowerPoint's Normal view shows off-slide objects; thumbnails clip to the slide).

use std::collections::HashMap;
use std::sync::Arc;

use deckcraft_geom::Rect;
use deckcraft_model::{Master, Presentation, ShapeId, Slide};
use deckcraft_render::{Image, Region, RenderOpts};
use egui::{ColorImage, TextureHandle, TextureOptions};

#[derive(Clone, PartialEq)]
struct Key {
    slide: usize,
    master: usize,
    size: (u32, u32),
    edit: bool,
    extra: u64,
    /// The pasteboard region the slide's drawing calls for (canvas only).
    want: Region,
}

struct Entry {
    key: Key,
    tex: TextureHandle,
    /// The region the texture covers (at least `key.want`).
    region: Region,
    // Keep the Arcs alive so their addresses can't be reused while cached.
    _slide: Arc<Slide>,
    _master: Option<Arc<Master>>,
    stamp: u64,
}

#[derive(Default)]
pub struct Textures {
    canvas: Option<Entry>,
    /// How far the canvas slide's shapes reach (from their boxes), for the slide and master `Arc`s
    /// held (kept alive so their addresses can't be reused).
    estimate: Option<(Arc<Slide>, Option<Arc<Master>>, Rect)>,
    /// How far the last canvas render of a slide (by index) actually drew: text spilling out of
    /// its box shows there.
    drawn: Option<(usize, Rect)>,
    /// Canvas renders so far (tests, performance checks).
    pub canvas_renders: u64,
    thumbs: HashMap<usize, Entry>,
    clock: u64,
    pub last_render_ms: f64,
}

pub fn to_color_image(img: &Image, grayscale: bool) -> ColorImage {
    let w = img.width as usize;
    let h = img.height as usize;
    let mut px = Vec::with_capacity(w * h);
    for c in img.pixels.as_chunks::<4>().0 {
        let (r, g, b, a) = (c[0], c[1], c[2], c[3]);
        if grayscale {
            let y = ((r as u32 * 77 + g as u32 * 150 + b as u32 * 29) >> 8) as u8;
            px.push(egui::Color32::from_rgba_premultiplied(y, y, y, a));
        } else {
            px.push(egui::Color32::from_rgba_premultiplied(r, g, b, a));
        }
    }
    px.resize(w * h, egui::Color32::TRANSPARENT);
    ColorImage::new([w, h], px)
}

fn master_of(p: &Presentation, slide: &Slide) -> Option<Arc<Master>> {
    p.masters.iter().find(|m| m.layout(slide.layout).is_some()).cloned()
}

fn same(a: &Option<Arc<Master>>, b: &Option<Arc<Master>>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

fn threads() -> u16 {
    if cfg!(target_arch = "wasm32") { 0 } else { std::thread::available_parallelism().map(|n| n.get().min(8) as u16).unwrap_or(0) }
}

impl Textures {
    /// The canvas texture for slide `index`, the slide being `size` pixels, and the region it
    /// covers: the slide, grown over the pasteboard to show what reaches off the slide within
    /// `visible` (the canvas, in pixels from the slide's top-left corner). `extra` changes with the
    /// GIF frames.
    #[allow(clippy::too_many_arguments)]
    pub fn slide(
        &mut self,
        ctx: &egui::Context,
        p: &Presentation,
        index: usize,
        size: (u32, u32),
        visible: Region,
        edit: bool,
        extra: u64,
        grayscale: bool,
        gif_times: &[(ShapeId, f64)],
    ) -> Option<(TextureHandle, Region)> {
        let slide = p.slides.get(index)?.clone();
        let master = master_of(p, &slide);
        let scale = size.0 as f64 / p.slide_size.width.max(1.0);
        let estimate = match &self.estimate {
            Some((s, m, r)) if Arc::ptr_eq(s, &slide) && same(m, &master) => *r,
            _ => {
                let r = deckcraft_render::slide_extent(p, index);
                self.estimate = Some((slide.clone(), master.clone(), r));
                r
            }
        };
        let reach = match self.drawn {
            Some((i, d)) if i == index => estimate.union(d),
            _ => estimate,
        };
        let want = deckcraft_render::pasteboard_region(size, scale, reach, visible);
        let key = Key {
            slide: Arc::as_ptr(&slide) as usize,
            master: master.as_ref().map(|m| Arc::as_ptr(m) as usize).unwrap_or(0),
            size,
            edit,
            extra: extra ^ grayscale as u64,
            want,
        };
        if let Some(e) = &self.canvas
            && e.key == key
        {
            return Some((e.tex.clone(), e.region));
        }
        let t0 = crate::now_ms();
        let render = |region: Region| {
            let opts =
                RenderOpts { scale, edit, gif_times, threads: threads(), offset: region.offset(), size: Some(region.size()), ..Default::default() };
            deckcraft_render::render_slide_drawn(p, index, &opts)
        };
        let (mut img, drawn) = render(want);
        self.canvas_renders += 1;
        let mut region = want;
        // Text spilling out of its box can reach further than the boxes said. Once drawn, the
        // region it calls for is known (and is next frame's key): render again only if it shows
        // more than this render covered.
        let key = Key { want: deckcraft_render::pasteboard_region(size, scale, estimate.union(drawn), visible), ..key };
        if !region.contains(&key.want) {
            region = key.want;
            img = render(region).0;
            self.canvas_renders += 1;
        }
        self.drawn = Some((index, drawn));
        self.last_render_ms = crate::now_ms() - t0;
        let ci = to_color_image(&img, grayscale);
        let tex = match self.canvas.take() {
            Some(mut e) => {
                e.tex.set(ci, TextureOptions::LINEAR);
                e.tex
            }
            None => ctx.load_texture("slide-canvas", ci, TextureOptions::LINEAR),
        };
        self.canvas = Some(Entry { key, tex: tex.clone(), region, _slide: slide, _master: master, stamp: 0 });
        Some((tex, region))
    }

    /// Force the canvas to re-render next time (fonts loaded, theme changed in place…).
    pub fn invalidate(&mut self) {
        self.canvas = None;
        self.estimate = None;
        self.drawn = None;
        self.thumbs.clear();
    }

    /// A thumbnail texture; renders at most `budget` new thumbnails per frame (returns None when
    /// it is over budget, so the caller draws a placeholder and asks for another frame).
    pub fn thumb(&mut self, ctx: &egui::Context, p: &Presentation, index: usize, width_px: u32, budget: &mut u32) -> Option<TextureHandle> {
        let slide = p.slides.get(index)?.clone();
        let master = master_of(p, &slide);
        let h = (width_px as f64 * p.slide_size.height / p.slide_size.width.max(1.0)).round().max(1.0) as u32;
        let key = Key {
            slide: Arc::as_ptr(&slide) as usize,
            master: master.as_ref().map(|m| Arc::as_ptr(m) as usize).unwrap_or(0),
            size: (width_px, h),
            edit: false,
            extra: 0,
            want: Region::slide(width_px, h),
        };
        self.clock += 1;
        let clock = self.clock;
        if let Some(e) = self.thumbs.get_mut(&key.slide)
            && e.key == key
        {
            e.stamp = clock;
            return Some(e.tex.clone());
        }
        if *budget == 0 {
            // Keep showing the stale thumbnail while the new one waits.
            return self.thumbs.get(&key.slide).map(|e| e.tex.clone());
        }
        *budget -= 1;
        let scale = width_px as f64 / p.slide_size.width.max(1.0);
        let img = deckcraft_render::render_slide(p, index, &RenderOpts { scale, size: Some((width_px, h)), threads: 0, ..Default::default() });
        let tex = ctx.load_texture(format!("thumb-{index}"), to_color_image(&img, false), TextureOptions::LINEAR);
        // Drop the old entry for this slide address; entries for slides that no longer exist age out.
        let region = key.want;
        self.thumbs.insert(key.slide, Entry { key, tex: tex.clone(), region, _slide: slide, _master: master, stamp: clock });
        if self.thumbs.len() > 600 {
            let mut v: Vec<(usize, u64)> = self.thumbs.iter().map(|(k, e)| (*k, e.stamp)).collect();
            v.sort_by_key(|x| x.1);
            for (k, _) in v.into_iter().take(200) {
                self.thumbs.remove(&k);
            }
        }
        Some(tex)
    }
}
