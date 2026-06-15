use crate::app::SwitchAppsState;
use crate::config::SwitchAppsColors;
use crate::utils::{check_error, get_moinitor_rect};

use anyhow::{anyhow, Context, Result};
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use windows::core::Interface;
use windows::Foundation::Size;
use windows::Graphics::DirectX::{DirectXAlphaMode, DirectXPixelFormat};
use windows::System::DispatcherQueueController;
use windows::UI::Color;
use windows::UI::Composition::Desktop::DesktopWindowTarget;
use windows::UI::Composition::{
    CompositionCapabilities, CompositionColorBrush, CompositionDrawingSurface,
    CompositionGraphicsDevice, Compositor, ContainerVisual, DropShadow,
    CompositionRoundedRectangleGeometry, SpriteVisual,
};
use windows::Win32::{
    Foundation::{HMODULE, HWND, POINT, SIZE},
    Graphics::{
        Direct2D::{
            Common::{
                D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_RECT_F, D2D_SIZE_U,
            },
            D2D1CreateDevice, ID2D1Device, ID2D1DeviceContext, D2D1_BITMAP_OPTIONS_NONE,
            D2D1_BITMAP_PROPERTIES1, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_PRIMITIVE_BLEND_COPY,
        },
        Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP},
        Direct3D11::{
            D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
        },
        Dxgi::{Common::DXGI_FORMAT_B8G8R8A8_UNORM, IDXGIDevice},
        Gdi::{
            CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetDIBits,
            GetObjectW, ReleaseDC, SelectObject, BITMAP, BITMAPINFO, BITMAPINFOHEADER, BI_RGB,
            DIB_RGB_COLORS, HDC,
        },
        GdiPlus::{
            FillModeAlternate, GdipAddPathArc, GdipClosePathFigure, GdipCreateBitmapFromScan0,
            GdipCreatePath, GdipCreatePen1, GdipCreateSolidFill, GdipDeleteBrush,
            GdipDeleteGraphics, GdipDeletePath, GdipDeletePen, GdipDisposeImage, GdipDrawImageRectI,
            GdipDrawPath, GdipFillPath, GdipGetImageGraphicsContext, GdipGraphicsClear,
            GdipSetInterpolationMode, GdipSetPixelOffsetMode, GdipSetSmoothingMode,
            GdiplusShutdown, GdiplusStartup, GdiplusStartupInput, GpBitmap, GpBrush, GpGraphics,
            GpImage, GpPath, GpPen, GpSolidFill, InterpolationModeHighQualityBicubic,
            PixelOffsetModeHighQuality, SmoothingModeAntiAlias, Unit,
        },
    },
    System::WinRT::{
        Composition::{
            ICompositionCapabilitiesInteropFactory, ICompositionDrawingSurfaceInterop,
            ICompositorDesktopInterop, ICompositorInterop,
        },
        CreateDispatcherQueueController, DispatcherQueueOptions, DQTAT_COM_STA,
        DQTYPE_THREAD_CURRENT,
    },
    UI::{
        HiDpi::GetDpiForWindow,
        Input::KeyboardAndMouse::SetFocus,
        WindowsAndMessaging::{
            GetCursorPos, GetIconInfo, SetWindowPos, ShowWindow, HICON, HWND_TOPMOST, ICONINFO,
            SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, SW_SHOWNA,
        },
    },
};
use windows_numerics::{Vector2, Vector3};

pub const ICON_SIZE_BASE: i32 = 64;
/// Margin between the pane edge and the icon list.
pub const WINDOW_BORDER_SIZE_BASE: i32 = 16;
/// Padding around each icon, i.e. the margin between apps in the list.
pub const ICON_BORDER_SIZE_BASE: i32 = 14;

// The pane background tint, bevel border (light/dark), and selection colors are
// configurable via the .ini; see `SwitchAppsColors` and its defaults in config.rs.
/// Border thickness in px @ 96dpi (scaled by DPI).
const BORDER_WIDTH_BASE: f32 = 1.0;
/// How far the border softens/blurs inward, in px @ 96dpi (scaled by DPI).
const BORDER_BLUR_BASE: f32 = 2.5;

/// Corner radius / shadow geometry, in px @ 96dpi (scaled by DPI at paint time).
const CORNER_RADIUS_BASE: f32 = 8.0;
const SHADOW_BLUR_BASE: f32 = 18.0;
const SHADOW_OFFSET_Y_BASE: f32 = 3.0;
const SHADOW_OPACITY: f32 = 0.5;

/// 32bpp premultiplied ARGB, the format the GDI+ strip rasterizer produces.
const PIXEL_FORMAT_32BPP_PARGB: i32 = 0x000e200b;

/// Windows.UI.Composition renderer for the Alt+Tab overlay: a host-backdrop acrylic
/// pane with rounded corners and a real composition drop shadow. The icon row is
/// rasterized with GDI+ and uploaded into a composition drawing surface.
pub struct GdiAAPainter {
    hwnd: HWND,
    hdc_screen: HDC,
    token: usize, // GdiplusStartup token
    show: bool,
    effects_supported: bool,
    last_size: (i32, i32),
    colors: SwitchAppsColors,

    // Composition visual tree (resized per paint). The compositor, target and graphics
    // device are held only to keep the tree/surface alive for the renderer's lifetime.
    _compositor: Compositor,
    _target: DesktopWindowTarget,
    _graphics_device: CompositionGraphicsDevice,
    drawing_surface: CompositionDrawingSurface,
    root: ContainerVisual,
    backdrop: SpriteVisual,
    pane: SpriteVisual,
    content: SpriteVisual,
    rounded_geo: CompositionRoundedRectangleGeometry,
    drop_shadow: DropShadow,
    tint_brush: CompositionColorBrush,

    // Kept alive for the lifetime of the renderer (declared so they drop last).
    _d3d_device: ID3D11Device,
    _d2d_device: ID2D1Device,
    _dispatcher_controller: DispatcherQueueController,
}

impl GdiAAPainter {
    pub fn new(hwnd: HWND, colors: SwitchAppsColors) -> Result<Self> {
        // A DispatcherQueue on this (message-loop) thread is required before the
        // Compositor; DQTAT_COM_STA initializes the apartment as STA.
        let dispatcher_controller = unsafe {
            CreateDispatcherQueueController(DispatcherQueueOptions {
                dwSize: std::mem::size_of::<DispatcherQueueOptions>() as u32,
                threadType: DQTYPE_THREAD_CURRENT,
                apartmentType: DQTAT_COM_STA,
            })
        }
        .context("Failed to create dispatcher queue controller")?;

        let compositor = Compositor::new().context("Failed to create compositor")?;

        // D3D11 -> DXGI -> D2D device, then a composition graphics device.
        let d3d_device = create_d3d_device().context("Failed to create D3D11 device")?;
        let dxgi: IDXGIDevice = d3d_device.cast()?;
        let d2d_device = unsafe { D2D1CreateDevice(&dxgi, None) }?;
        let comp_interop: ICompositorInterop = compositor.cast()?;
        let graphics_device = unsafe { comp_interop.CreateGraphicsDevice(&d2d_device) }?;

        // Host the visual tree on the window.
        let desktop_interop: ICompositorDesktopInterop = compositor.cast()?;
        let target = unsafe { desktop_interop.CreateDesktopWindowTarget(hwnd, true) }?;

        // Build the tree:  root -> [ backdrop (blur), pane (tint + shadow) -> content (icons) ]
        let root = compositor.CreateContainerVisual()?;
        let backdrop = compositor.CreateSpriteVisual()?;
        let pane = compositor.CreateSpriteVisual()?;
        let content = compositor.CreateSpriteVisual()?;

        let backdrop_brush = compositor.CreateHostBackdropBrush()?;
        backdrop.SetBrush(&backdrop_brush)?;

        let tint_brush = compositor.CreateColorBrushWithColor(argb_color(colors.background))?;
        pane.SetBrush(&tint_brush)?;

        let drawing_surface = graphics_device.CreateDrawingSurface(
            Size {
                Width: 1.0,
                Height: 1.0,
            },
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            DirectXAlphaMode::Premultiplied,
        )?;
        let surface_brush = compositor.CreateSurfaceBrushWithSurface(&drawing_surface)?;
        content.SetBrush(&surface_brush)?;

        // Rounded clip applied to both the blur and the pane (host backdrop can ignore
        // a parent clip, so clip each sprite directly).
        let rounded_geo = compositor.CreateRoundedRectangleGeometry()?;
        backdrop.SetClip(&compositor.CreateGeometricClipWithGeometry(&rounded_geo)?)?;
        pane.SetClip(&compositor.CreateGeometricClipWithGeometry(&rounded_geo)?)?;

        // Rounded composition drop shadow, derived from the pane's tinted silhouette.
        let drop_shadow = compositor.CreateDropShadow()?;
        drop_shadow.SetColor(argb_color(0xff000000))?;
        drop_shadow.SetOpacity(SHADOW_OPACITY)?;
        pane.SetShadow(&drop_shadow)?;

        pane.Children()?.InsertAtTop(&content)?;
        let root_children = root.Children()?;
        root_children.InsertAtBottom(&backdrop)?;
        root_children.InsertAtTop(&pane)?;
        root.SetIsVisible(false)?;
        target.SetRoot(&root)?;

        // Are composition effects available (transparency on, not battery saver, ...)?
        // Diagnostic: force on to test the host-backdrop blur directly.
        let _ = query_effects_supported(hwnd);
        let effects_supported = true;

        // Acquire the GDI resources last: they are freed by Self's Drop, which only
        // runs once Self is constructed, so any `?` failure above would otherwise leak
        // them. GDI+ rasterizes the icon strip; the screen DC backs the per-paint DIB.
        // Nothing between here and `Ok(Self)` is fallible, so neither can leak.
        let startup_input = GdiplusStartupInput {
            GdiplusVersion: 1,
            ..Default::default()
        };
        let mut token: usize = 0;
        check_error(|| unsafe { GdiplusStartup(&mut token, &startup_input, std::ptr::null_mut()) })
            .context("Failed to initialize GDI+")?;
        let hdc_screen = unsafe { GetDC(None) };

        Ok(Self {
            hwnd,
            hdc_screen,
            token,
            show: false,
            effects_supported,
            last_size: (0, 0),
            colors,
            _compositor: compositor,
            _target: target,
            _graphics_device: graphics_device,
            drawing_surface,
            root,
            backdrop,
            pane,
            content,
            rounded_geo,
            drop_shadow,
            tint_brush,
            _d3d_device: d3d_device,
            _d2d_device: d2d_device,
            _dispatcher_controller: dispatcher_controller,
        })
    }

    pub fn paint(&mut self, state: &SwitchAppsState) {
        let dpi_scale = get_dpi_scale(self.hwnd);
        let icon_size_max = (ICON_SIZE_BASE as f64 * dpi_scale) as i32;
        let border_size = (WINDOW_BORDER_SIZE_BASE as f64 * dpi_scale) as i32;
        let icon_border = (ICON_BORDER_SIZE_BASE as f64 * dpi_scale) as i32;

        let Coordinate {
            x,
            y,
            width,
            height,
            icon_size,
            item_size,
        } = Coordinate::new(
            state.apps.len() as i32,
            icon_size_max,
            border_size,
            icon_border,
        );

        // Shadow margin: room around the pane for the blurred shadow.
        let blur = (SHADOW_BLUR_BASE * dpi_scale as f32).round();
        let margin = (blur + 12.0 * dpi_scale as f32).round() as i32;

        // Position + size the window (was UpdateLayeredWindow's job).
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                x - margin,
                y - margin,
                width + 2 * margin,
                height + 2 * margin,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }

        if let Err(err) = self.layout(width, height, margin, dpi_scale) {
            error!("composition layout failed: {err}");
        }
        if let Err(err) = self.rasterize_and_upload(state, width, height, icon_size, item_size,
            border_size, icon_border, dpi_scale)
        {
            error!("strip upload failed: {err}");
        }

        if !self.show {
            let _ = self.root.SetIsVisible(true);
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_SHOWNA);
                let _ = SetFocus(Some(self.hwnd));
            }
            self.show = true;
        }
    }

    /// Resizes the visual tree to the current pane size (only when it changed).
    fn layout(&mut self, width: i32, height: i32, margin: i32, dpi_scale: f64) -> Result<()> {
        if self.last_size == (width, height) {
            return Ok(());
        }
        let w = width as f32;
        let h = height as f32;
        let m = margin as f32;

        self.root.SetSize(Vector2 {
            X: w + 2.0 * m,
            Y: h + 2.0 * m,
        })?;
        let pane_size = Vector2 { X: w, Y: h };
        let pane_offset = Vector3 { X: m, Y: m, Z: 0.0 };
        self.backdrop.SetSize(pane_size)?;
        self.backdrop.SetOffset(pane_offset)?;
        self.pane.SetSize(pane_size)?;
        self.pane.SetOffset(pane_offset)?;
        self.content.SetSize(pane_size)?;

        self.rounded_geo.SetSize(pane_size)?;
        let r = CORNER_RADIUS_BASE * dpi_scale as f32;
        self.rounded_geo.SetCornerRadius(Vector2 { X: r, Y: r })?;

        self.drop_shadow
            .SetBlurRadius(SHADOW_BLUR_BASE * dpi_scale as f32)?;
        self.drop_shadow.SetOffset(Vector3 {
            X: 0.0,
            Y: SHADOW_OFFSET_Y_BASE * dpi_scale as f32,
            Z: 0.0,
        })?;

        // Host-backdrop blur renders opaque black on Win32 desktop windows, so the blur
        // layer stays hidden; the pane is a translucent frosted tint over the desktop.
        let _ = self.effects_supported;
        self.backdrop.SetIsVisible(false)?;
        self.tint_brush.SetColor(argb_color(self.colors.background))?;

        let si: ICompositionDrawingSurfaceInterop = self.drawing_surface.cast()?;
        unsafe { si.Resize(SIZE { cx: width, cy: height })? };
        self.last_size = (width, height);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn rasterize_and_upload(
        &self,
        state: &SwitchAppsState,
        width: i32,
        height: i32,
        icon_size: i32,
        item_size: i32,
        border_size: i32,
        icon_border: i32,
        dpi_scale: f64,
    ) -> Result<()> {
        unsafe {
            // 1) Rasterize the icon row + selection + border into a premultiplied DIB.
            let hdc_mem = CreateCompatibleDC(Some(self.hdc_screen));
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let bitmap_mem =
                match CreateDIBSection(Some(self.hdc_screen), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
                {
                    Ok(v) => v,
                    Err(_) => {
                        let _ = DeleteDC(hdc_mem);
                        return Err(anyhow!("CreateDIBSection failed"));
                    }
                };
            let old_bitmap = SelectObject(hdc_mem, bitmap_mem.into());

            let mut bitmap_ptr: *mut GpBitmap = std::ptr::null_mut();
            GdipCreateBitmapFromScan0(
                width,
                height,
                width * 4,
                PIXEL_FORMAT_32BPP_PARGB,
                Some(bits as *const u8),
                &mut bitmap_ptr,
            );
            let image_ptr: *mut GpImage = bitmap_ptr as *mut GpImage;
            let mut g: *mut GpGraphics = std::ptr::null_mut();
            GdipGetImageGraphicsContext(image_ptr, &mut g);
            GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
            GdipSetInterpolationMode(g, InterpolationModeHighQualityBicubic);
            GdipSetPixelOffsetMode(g, PixelOffsetModeHighQuality);

            // Transparent background: the tint + blur come from composition layers.
            GdipGraphicsClear(g, 0x00000000);

            // Rounded 1px stroke that follows the pane's rounded corners (a rectangular
            // border would be clipped at the corners by the composition rounded clip).
            let pen_width = (BORDER_WIDTH_BASE * dpi_scale as f32).max(1.0);
            let w = width as f32;
            let h = height as f32;
            // Centered stroke inset by half its width so its OUTER edge sits flush on the
            // pane edge (no sliver of tint showing outside the border).
            let inset = pen_width * 0.5;
            let corner = CORNER_RADIUS_BASE * dpi_scale as f32;
            let blur = BORDER_BLUR_BASE * dpi_scale as f32;
            stroke_round_rect(
                g,
                self.colors.border_light,
                self.colors.border_dark,
                inset,
                inset,
                w - 2.0 * inset,
                h - 2.0 * inset,
                (corner - inset).max(0.0),
                pen_width,
                blur,
            );

            // Rounded selection highlight behind the selected app.
            let sel_left = (border_size + item_size * state.index as i32) as f32;
            let sel_top = border_size as f32;
            let sel_radius = (item_size as f32 / 4.0).min(8.0 * dpi_scale as f32);
            fill_round_rect(
                g,
                self.colors.selection,
                sel_left,
                sel_top,
                item_size as f32,
                item_size as f32,
                sel_radius,
            );

            // Icons. Build a correct premultiplied bitmap ourselves (GdipCreateBitmapFromHICON
            // ignores the AND-mask for non-alpha icons and fills them with opaque black).
            for (i, (icon, _)) in state.apps.iter().enumerate() {
                let Some((iw, ih, buf)) = hicon_to_pbgra(self.hdc_screen, *icon) else {
                    continue;
                };
                let mut icon_ptr: *mut GpBitmap = std::ptr::null_mut();
                if GdipCreateBitmapFromScan0(
                    iw,
                    ih,
                    iw * 4,
                    PIXEL_FORMAT_32BPP_PARGB,
                    Some(buf.as_ptr()),
                    &mut icon_ptr,
                )
                .0 != 0
                    || icon_ptr.is_null()
                {
                    continue;
                }
                let ix = border_size + item_size * (i as i32) + icon_border;
                let iy = border_size + icon_border;
                GdipDrawImageRectI(g, icon_ptr as *mut GpImage, ix, iy, icon_size, icon_size);
                GdipDisposeImage(icon_ptr as *mut GpImage);
                // `buf` must outlive the draw above (GdipCreateBitmapFromScan0 does not copy).
                drop(buf);
            }
            GdipDeleteGraphics(g);
            GdipDisposeImage(image_ptr);

            // 2) Upload the DIB into the composition drawing surface via D2D.
            let upload = self.upload_surface(bits, width, height);

            SelectObject(hdc_mem, old_bitmap);
            let _ = DeleteObject(bitmap_mem.into());
            let _ = DeleteDC(hdc_mem);
            upload
        }
    }

    unsafe fn upload_surface(&self, bits: *mut c_void, width: i32, height: i32) -> Result<()> {
        unsafe {
            let si: ICompositionDrawingSurfaceInterop = self.drawing_surface.cast()?;
            let mut offset = POINT::default();
            let dc: ID2D1DeviceContext = si.BeginDraw(None, &mut offset)?;

            let props = D2D1_BITMAP_PROPERTIES1 {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: 96.0,
                dpiY: 96.0,
                bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
                colorContext: ManuallyDrop::new(None),
            };
            let bitmap = dc.CreateBitmap(
                D2D_SIZE_U {
                    width: width as u32,
                    height: height as u32,
                },
                Some(bits as *const c_void),
                (width * 4) as u32,
                &props,
            )?;

            // COPY blend so transparent source pixels overwrite stale surface content.
            dc.SetPrimitiveBlend(D2D1_PRIMITIVE_BLEND_COPY);
            let dest = D2D_RECT_F {
                left: offset.x as f32,
                top: offset.y as f32,
                right: (offset.x + width) as f32,
                bottom: (offset.y + height) as f32,
            };
            dc.DrawBitmap(
                &bitmap,
                Some(&dest),
                1.0,
                D2D1_INTERPOLATION_MODE_LINEAR,
                None,
                None,
            );
            si.EndDraw()?;
        }
        Ok(())
    }

    pub fn unpaint(&mut self, _state: SwitchAppsState) {
        let _ = self.root.SetIsVisible(false);
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
        self.show = false;
    }

    pub fn find_clicked_app_index(&self, state: &SwitchAppsState) -> Option<usize> {
        let cursor_pos = unsafe {
            let mut pos = POINT::default();
            let _ = GetCursorPos(&mut pos);
            pos
        };

        let dpi_scale = get_dpi_scale(self.hwnd);
        let icon_size_max = (ICON_SIZE_BASE as f64 * dpi_scale) as i32;
        let border_size = (WINDOW_BORDER_SIZE_BASE as f64 * dpi_scale) as i32;
        let icon_border = (ICON_BORDER_SIZE_BASE as f64 * dpi_scale) as i32;

        let Coordinate {
            x, y, item_size, ..
        } = Coordinate::new(
            state.apps.len() as i32,
            icon_size_max,
            border_size,
            icon_border,
        );

        let xpos = cursor_pos.x - x;
        let ypos = cursor_pos.y - y;

        let cy = border_size;
        for (i, _) in state.apps.iter().enumerate() {
            let cx = border_size + item_size * (i as i32);
            if xpos >= cx && xpos < cx + item_size && ypos >= cy && ypos < cy + item_size {
                return Some(i);
            }
        }
        None
    }
}

impl Drop for GdiAAPainter {
    fn drop(&mut self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
            ReleaseDC(None, self.hdc_screen);
            GdiplusShutdown(self.token);
        }
        // Composition COM objects + dispatcher controller drop via field order.
    }
}

/// Converts a packed `0xAARRGGBB` value into a composition `Color`.
fn argb_color(argb: u32) -> Color {
    Color {
        A: (argb >> 24) as u8,
        R: (argb >> 16) as u8,
        G: (argb >> 8) as u8,
        B: argb as u8,
    }
}

/// Creates a hardware D3D11 device (with BGRA support for D2D), falling back to WARP.
fn create_d3d_device() -> Result<ID3D11Device> {
    for driver in [D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP] {
        let mut device: Option<ID3D11Device> = None;
        let hr = unsafe {
            D3D11CreateDevice(
                None,
                driver,
                HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
        };
        if hr.is_ok() {
            if let Some(device) = device {
                return Ok(device);
            }
        }
    }
    Err(anyhow!("D3D11CreateDevice failed for hardware and WARP"))
}

fn query_effects_supported(hwnd: HWND) -> Result<bool> {
    let factory: ICompositionCapabilitiesInteropFactory =
        windows::core::factory::<CompositionCapabilities, ICompositionCapabilitiesInteropFactory>()?;
    let caps = unsafe { factory.GetForWindow(hwnd)? };
    Ok(caps.AreEffectsSupported()?)
}

/// Strokes a beveled rounded-rectangle outline: the top + left edges (and top-left corner)
/// in `light_color`, the bottom + right edges (and bottom-right corner) in `dark_color`,
/// splitting the two off-corners at 45°. Gives the panel a sense of depth (lit top-left).
#[allow(clippy::too_many_arguments)]
unsafe fn stroke_round_rect(
    g: *mut GpGraphics,
    light_color: u32,
    dark_color: u32,
    left: f32,
    top: f32,
    w: f32,
    h: f32,
    radius: f32,
    pen_width: f32,
    blur: f32,
) {
    unsafe {
        let d = radius * 2.0;
        let right = left + w;
        let bottom = top + h;

        // Light "L": bottom-left corner (from its 45° midpoint) up the left edge, around the
        // top-left corner, across the top edge, to the top-right corner's 45° midpoint.
        let mut light_path: *mut GpPath = std::ptr::null_mut();
        GdipCreatePath(FillModeAlternate, &mut light_path);
        GdipAddPathArc(light_path, left, bottom - d, d, d, 135.0, 45.0);
        GdipAddPathArc(light_path, left, top, d, d, 180.0, 90.0);
        GdipAddPathArc(light_path, right - d, top, d, d, 270.0, 45.0);
        stroke_path_soft(g, light_path, light_color, pen_width, blur);
        GdipDeletePath(light_path);

        // Dark "L": top-right 45° midpoint, down the right edge, around the bottom-right
        // corner, across the bottom edge, to the bottom-left corner's 45° midpoint.
        let mut dark_path: *mut GpPath = std::ptr::null_mut();
        GdipCreatePath(FillModeAlternate, &mut dark_path);
        GdipAddPathArc(dark_path, right - d, top, d, d, 315.0, 45.0);
        GdipAddPathArc(dark_path, right - d, bottom - d, d, d, 0.0, 90.0);
        GdipAddPathArc(dark_path, left, bottom - d, d, d, 90.0, 45.0);
        stroke_path_soft(g, dark_path, dark_color, pen_width, blur);
        GdipDeletePath(dark_path);
    }
}

/// Strokes a path several times — a wide faint pass plus narrower stronger ones — to fake
/// a soft, blurred edge instead of a hard 1px line.
unsafe fn stroke_path_soft(
    g: *mut GpGraphics,
    path: *mut GpPath,
    color_argb: u32,
    core_width: f32,
    blur: f32,
) {
    unsafe {
        let rgb = color_argb & 0x00ff_ffff;
        let base_a = ((color_argb >> 24) & 0xff) as f32;
        // (extra width, alpha factor), widest+faintest first so the core lands on top.
        for (extra, factor) in [(blur * 2.0, 0.22f32), (blur, 0.45), (0.0, 1.0)] {
            let a = (base_a * factor).round() as u32;
            let mut pen: *mut GpPen = std::ptr::null_mut();
            GdipCreatePen1((a << 24) | rgb, core_width + extra, Unit(2), &mut pen);
            GdipDrawPath(g, pen, path);
            GdipDeletePen(pen);
        }
    }
}

/// Fills an antialiased rounded rectangle with a solid ARGB color (GDI+).
unsafe fn fill_round_rect(
    g: *mut GpGraphics,
    color_argb: u32,
    left: f32,
    top: f32,
    w: f32,
    h: f32,
    radius: f32,
) {
    unsafe {
        let d = radius * 2.0;
        let right = left + w;
        let bottom = top + h;
        let mut brush: *mut GpSolidFill = std::ptr::null_mut();
        GdipCreateSolidFill(color_argb, &mut brush);
        let mut path: *mut GpPath = std::ptr::null_mut();
        GdipCreatePath(FillModeAlternate, &mut path);
        GdipAddPathArc(path, left, top, d, d, 180.0, 90.0);
        GdipAddPathArc(path, right - d, top, d, d, 270.0, 90.0);
        GdipAddPathArc(path, right - d, bottom - d, d, d, 0.0, 90.0);
        GdipAddPathArc(path, left, bottom - d, d, d, 90.0, 90.0);
        GdipClosePathFigure(path);
        GdipFillPath(g, brush as *mut GpBrush, path);
        GdipDeletePath(path);
        GdipDeleteBrush(brush as *mut GpBrush);
    }
}

/// Extracts an HICON into a premultiplied BGRA buffer (`width, height, pixels`), using
/// the icon's alpha channel when present and otherwise deriving alpha from its AND-mask.
/// This is the correct path that `GdipCreateBitmapFromHICON` gets wrong for mask icons.
unsafe fn hicon_to_pbgra(hdc: HDC, hicon: HICON) -> Option<(i32, i32, Vec<u8>)> {
    unsafe {
        let mut info = ICONINFO::default();
        if GetIconInfo(hicon, &mut info).is_err() {
            return None;
        }
        let color = info.hbmColor;
        let mask = info.hbmMask;

        let cleanup = || {
            let _ = DeleteObject(color.into());
            let _ = DeleteObject(mask.into());
        };

        let mut bmp = BITMAP::default();
        if GetObjectW(
            color.into(),
            std::mem::size_of::<BITMAP>() as i32,
            Some(&mut bmp as *mut _ as *mut c_void),
        ) == 0
        {
            cleanup();
            return None;
        }
        let (w, h) = (bmp.bmWidth, bmp.bmHeight);
        if w <= 0 || h <= 0 {
            cleanup();
            return None;
        }

        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                biHeight: -h, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let count = (w * h) as usize;
        let mut buf = vec![0u8; count * 4];
        if GetDIBits(
            hdc,
            color,
            0,
            h as u32,
            Some(buf.as_mut_ptr() as *mut c_void),
            &mut bmi,
            DIB_RGB_COLORS,
        ) == 0
        {
            cleanup();
            return None;
        }

        // No alpha channel? Derive it from the AND-mask (mask black = opaque).
        if !buf.chunks_exact(4).any(|p| p[3] != 0) {
            let mut mask_buf = vec![0u8; count * 4];
            if GetDIBits(
                hdc,
                mask,
                0,
                h as u32,
                Some(mask_buf.as_mut_ptr() as *mut c_void),
                &mut bmi,
                DIB_RGB_COLORS,
            ) != 0
            {
                for (p, m) in buf.chunks_exact_mut(4).zip(mask_buf.chunks_exact(4)) {
                    p[3] = if m[0] == 0 { 255 } else { 0 };
                }
            }
        }

        // Premultiply for the PARGB target surface.
        for p in buf.chunks_exact_mut(4) {
            let a = p[3] as u32;
            p[0] = (p[0] as u32 * a / 255) as u8;
            p[1] = (p[1] as u32 * a / 255) as u8;
            p[2] = (p[2] as u32 * a / 255) as u8;
        }

        cleanup();
        Some((w, h, buf))
    }
}

fn get_dpi_scale(hwnd: HWND) -> f64 {
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        if dpi == 0 {
            1.0
        } else {
            dpi as f64 / 96.0
        }
    }
}

struct Coordinate {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    icon_size: i32,
    item_size: i32,
}

impl Coordinate {
    fn new(num_apps: i32, icon_size_max: i32, border_size: i32, icon_border: i32) -> Self {
        let monitor_rect = get_moinitor_rect();
        let monitor_width = monitor_rect.right - monitor_rect.left;
        let monitor_height = monitor_rect.bottom - monitor_rect.top;

        let icon_size =
            ((monitor_width - 2 * border_size) / num_apps - icon_border * 2).min(icon_size_max);

        let item_size = icon_size + icon_border * 2;
        let width = item_size * num_apps + border_size * 2;
        let height = item_size + border_size * 2;
        let x = monitor_rect.left + (monitor_width - width) / 2;
        let y = monitor_rect.top + (monitor_height - height) / 2;

        Self {
            x,
            y,
            width,
            height,
            icon_size,
            item_size,
        }
    }
}
