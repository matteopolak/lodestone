//! The faces of a set of placed cells: the edge-update order shared by trees and templates.

use crate::blocks::Dir;
use crate::pos::Pos;

/// Every face where the filled region `shape` (x-major, then y, then z over `lo..=hi`) meets
/// empty space, in the reference's face order: runs along z first, then along y, then along x;
/// each run lists its entering face and its leaving face. The position is the filled cell.
#[must_use]
pub fn edge_faces(lo: Pos, hi: Pos, shape: &[bool]) -> Vec<(Dir, Pos)> {
    let (sx, sy, sz) = ((hi.x - lo.x + 1) as usize, (hi.y - lo.y + 1) as usize, (hi.z - lo.z + 1) as usize);
    let full = |x: usize, y: usize, z: usize| shape[(x * sy + y) * sz + z];
    let at = |x: usize, y: usize, z: usize| Pos::new(lo.x + x as i32, lo.y + y as i32, lo.z + z as i32);
    let mut faces: Vec<(Dir, Pos)> = Vec::new();
    for a in 0..sx {
        for b in 0..sy {
            let mut last = false;
            for c in 0..=sz {
                let f = c != sz && full(a, b, c);
                if !last && f {
                    faces.push((Dir::North, at(a, b, c)));
                }
                if last && !f {
                    faces.push((Dir::South, at(a, b, c - 1)));
                }
                last = f;
            }
        }
    }
    for a in 0..sz {
        for b in 0..sx {
            let mut last = false;
            for c in 0..=sy {
                let f = c != sy && full(b, c, a);
                if !last && f {
                    faces.push((Dir::Down, at(b, c, a)));
                }
                if last && !f {
                    faces.push((Dir::Up, at(b, c - 1, a)));
                }
                last = f;
            }
        }
    }
    for a in 0..sy {
        for b in 0..sz {
            let mut last = false;
            for c in 0..=sx {
                let f = c != sx && full(c, a, b);
                if !last && f {
                    faces.push((Dir::West, at(c, a, b)));
                }
                if last && !f {
                    faces.push((Dir::East, at(c - 1, a, b)));
                }
                last = f;
            }
        }
    }
    faces
}
