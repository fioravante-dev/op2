// Gera o ícone do app (.ico) e embute no op2.exe.
use std::path::Path;

/// Quadrado arredondado escuro com um anel verde ("O"), com antialiasing.
fn pixel(n: u32, x: u32, y: u32) -> [u8; 4] {
    let s = n as f32;
    let (u, v) = ((x as f32 + 0.5) / s - 0.5, (y as f32 + 0.5) / s - 0.5);
    let aa = 1.0 / s;
    let r = 0.2;
    let (qx, qy) = (u.abs() - (0.5 - r), v.abs() - (0.5 - r));
    let sq = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r;
    let bg_alpha = (0.5 - sq / aa).clamp(0.0, 1.0);
    let d = (u * u + v * v).sqrt();
    let ring = ((0.33 - d) / aa + 0.5).clamp(0.0, 1.0).min(((d - 0.19) / aa + 0.5).clamp(0.0, 1.0));
    let (bg, fg) = ([15.0, 26.0, 43.0], [43.0, 196.0, 138.0]);
    let c = |i: usize| (bg[i] * (1.0 - ring) + fg[i] * ring) as u8;
    [c(0), c(1), c(2), (bg_alpha * 255.0) as u8]
}

fn make_ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes
        .iter()
        .map(|&n| {
            let mut img = Vec::new();
            let mask_row = n.div_ceil(32) * 4;
            // BITMAPINFOHEADER (altura dobrada = cor + máscara)
            for v in [40u32, n, n * 2] {
                img.extend_from_slice(&v.to_le_bytes());
            }
            img.extend_from_slice(&1u16.to_le_bytes());
            img.extend_from_slice(&32u16.to_le_bytes());
            for v in [0u32, n * n * 4 + mask_row * n, 0, 0, 0, 0] {
                img.extend_from_slice(&v.to_le_bytes());
            }
            for y in (0..n).rev() {
                for x in 0..n {
                    let [r, g, b, a] = pixel(n, x, y);
                    img.extend_from_slice(&[b, g, r, a]);
                }
            }
            img.resize(img.len() + (mask_row * n) as usize, 0);
            img
        })
        .collect();

    let mut ico = Vec::new();
    for v in [0u16, 1, sizes.len() as u16] {
        ico.extend_from_slice(&v.to_le_bytes());
    }
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&n, img) in sizes.iter().zip(&images) {
        let dim = if n >= 256 { 0 } else { n as u8 };
        ico.extend_from_slice(&[dim, dim, 0, 0]);
        ico.extend_from_slice(&1u16.to_le_bytes());
        ico.extend_from_slice(&32u16.to_le_bytes());
        ico.extend_from_slice(&(img.len() as u32).to_le_bytes());
        ico.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for img in images {
        ico.extend_from_slice(&img);
    }
    ico
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let out = std::env::var("OUT_DIR").unwrap();
    let ico = Path::new(&out).join("op2.ico");
    std::fs::write(&ico, make_ico(&[16, 24, 32, 48, 64, 256])).unwrap();
    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().unwrap());
    res.set("FileDescription", "OP2 - builds do OP.gg no LoL");
    res.set("ProductName", "OP2");
    res.compile().expect("falha ao embutir o ícone");
}
