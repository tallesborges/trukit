//! Native UnixFS merkleization — the in-process replacement for the Kubo
//! `ipfs add` / `dag export` shell-out. Reproduces
//! `ipfs add -r --cid-version=1 --raw-leaves --hidden`: CIDv1, raw leaves,
//! sha2-256, a 256 KiB balanced chunker, and a wrap-with-directory root, so the
//! content CID matches Kubo (golden-tested against the crate's pinned Kubo-0.40.1
//! vectors). Produces the full block set ready for Bulletin upload — no `ipfs`
//! binary required.

use crate::chain::{self, PreparedBlock};
use anyhow::{anyhow, bail, Context, Result};
use ipld_core::cid::{Cid as IpldCid, Version};
use rust_unixfs::dir::builder::{BufferingTreeBuilder, TreeOptions};
use rust_unixfs::file::adder::FileAdder;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// dag-pb IPLD codec.
const DAG_PB: u64 = 0x70;
/// sha2-256 multihash code.
const SHA2_256: u64 = 0x12;
/// Chain-enforced `MaxTransactionSize` (2 MiB); a single block must fit one extrinsic.
const MAX_TRANSACTION_SIZE: usize = 2 * 1024 * 1024;

/// A merkleized directory: the root content CID plus every IPLD block of its DAG,
/// deduplicated and ready to store on Bulletin.
pub struct Merkleized {
    pub root: cid::Cid,
    pub blocks: Vec<PreparedBlock>,
}

/// Merkleize a build directory into a UnixFS DAG that resolves on IPFS gateways,
/// matching Kubo's default CIDv1 layout. Hidden files are included (like
/// `ipfs add --hidden`); entries are added in lexicographic path order.
pub fn merkleize_dir(dir: &str) -> Result<Merkleized> {
    let root_path = Path::new(dir);
    if !root_path.is_dir() {
        bail!("{dir} is not a directory");
    }

    let mut files = Vec::new();
    collect_files(root_path, root_path, &mut files)?;
    if files.is_empty() {
        bail!("{dir} contains no files to deploy");
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));

    // Accumulate every block (file leaves/roots + directory nodes), keyed by CID
    // so shared content (e.g. many empty files) is stored once.
    let mut seen: HashSet<IpldCid> = HashSet::new();
    let mut raw_blocks: Vec<(IpldCid, Vec<u8>)> = Vec::new();

    let mut opts = TreeOptions::default();
    opts.cid_version(Version::V1);
    opts.wrap_with_directory();
    let mut tree = BufferingTreeBuilder::new(opts);

    for (rel, abs) in &files {
        let data =
            std::fs::read(abs).with_context(|| format!("reading {}", abs.display()))?;
        let (file_cid, total_size) = add_file(&data, &mut seen, &mut raw_blocks)?;
        tree.put_link(rel, file_cid, total_size)
            .map_err(|e| anyhow!("linking {rel} into the directory tree: {e}"))?;
    }

    for node in tree.build() {
        let node = node.map_err(|e| anyhow!("building the directory DAG: {e}"))?;
        if seen.insert(node.cid) {
            raw_blocks.push((node.cid, node.block.into_vec()));
        }
    }

    // The root is the one node referenced by no other node's dag-pb links.
    let referenced: HashSet<IpldCid> = raw_blocks
        .iter()
        .filter(|(cid, _)| cid.codec() == DAG_PB)
        .flat_map(|(_, block)| dagpb_links(block))
        .collect();
    let root_ipld = raw_blocks
        .iter()
        .map(|(cid, _)| *cid)
        .find(|cid| !referenced.contains(cid))
        .context("merkleized DAG has no unreferenced root node")?;

    let mut blocks = Vec::with_capacity(raw_blocks.len());
    for (cid, data) in raw_blocks {
        if cid.hash().code() != SHA2_256 {
            bail!(
                "block {cid} uses multihash 0x{:x}; native merkleization only emits sha2-256",
                cid.hash().code()
            );
        }
        if data.len() > MAX_TRANSACTION_SIZE {
            bail!(
                "block {cid} is {} bytes, exceeding the chain's 2 MiB MaxTransactionSize",
                data.len()
            );
        }
        let content_hash = chain::content_hash(&data);
        blocks.push(PreparedBlock {
            codec: cid.codec(),
            data,
            content_hash,
        });
    }

    Ok(Merkleized {
        root: to_dotkit_cid(&root_ipld)?,
        blocks,
    })
}

/// Chunk a file with the default (256 KiB, CIDv1 raw-leaf) FileAdder, appending
/// its blocks to `raw_blocks`. Returns the file's root CID and cumulative dag-pb
/// size (the `Tsize` its parent link records).
fn add_file(
    data: &[u8],
    seen: &mut HashSet<IpldCid>,
    raw_blocks: &mut Vec<(IpldCid, Vec<u8>)>,
) -> Result<(IpldCid, u64)> {
    let mut adder = FileAdder::builder().with_cid_version(Version::V1).build();

    let mut file_blocks: Vec<(IpldCid, Vec<u8>)> = Vec::new();
    let mut pushed = 0;
    while pushed < data.len() {
        let (produced, consumed) = adder.push(&data[pushed..]);
        file_blocks.extend(produced);
        pushed += consumed;
    }
    file_blocks.extend(adder.finish());

    let total_size: u64 = file_blocks.iter().map(|(_, b)| b.len() as u64).sum();
    let root = file_blocks
        .last()
        .context("file produced no blocks")?
        .0;

    for (cid, block) in file_blocks {
        if seen.insert(cid) {
            raw_blocks.push((cid, block));
        }
    }
    Ok((root, total_size))
}

/// Recursively collect files under `base`, producing `(relative_path, absolute_path)`
/// pairs with `/`-separated relative paths. Hidden files are included.
fn collect_files(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading directory {}", dir.display()))?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<_>>()
        .with_context(|| format!("listing {}", dir.display()))?;
    entries.sort();

    for path in entries {
        if path.is_dir() {
            collect_files(base, &path, out)?;
        } else {
            let rel = path
                .strip_prefix(base)
                .expect("entry is under base")
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            out.push((rel, path));
        }
    }
    Ok(())
}

/// Child link CIDs referenced by a dag-pb node (each `PBLink.Hash`, field 1 of the
/// repeated `Links`, field 2 of the `PBNode`). A minimal protobuf walk — enough to
/// find which blocks are referenced so the unreferenced root can be identified.
fn dagpb_links(block: &[u8]) -> Vec<IpldCid> {
    fn varint(b: &[u8], i: &mut usize) -> u64 {
        let (mut v, mut shift) = (0u64, 0);
        while *i < b.len() {
            let byte = b[*i];
            *i += 1;
            v |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                break;
            }
            shift += 7;
        }
        v
    }

    let mut out = Vec::new();
    let mut i = 0;
    while i < block.len() {
        let field = varint(block, &mut i) >> 3;
        let len = varint(block, &mut i) as usize;
        if i + len > block.len() {
            break;
        }
        let chunk = &block[i..i + len];
        i += len;
        if field == 2 {
            let mut j = 0;
            while j < chunk.len() {
                let ltag = varint(chunk, &mut j);
                let llen = varint(chunk, &mut j) as usize;
                if j + llen > chunk.len() {
                    break;
                }
                if ltag >> 3 == 1 {
                    if let Ok(cid) = IpldCid::try_from(&chunk[j..j + llen]) {
                        out.push(cid);
                    }
                }
                j += llen;
            }
        }
    }
    out
}

/// Convert an `ipld-core` CID into the `cid` crate CID the rest of dotkit uses.
fn to_dotkit_cid(c: &IpldCid) -> Result<cid::Cid> {
    cid::Cid::try_from(c.to_bytes().as_slice()).context("converting merkleized root CID")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Golden vector from `rust-unixfs`' pinned Kubo-0.40.1 interop suite: a
    /// directory with `a.txt` = "a" and `b.txt` = "bb" merkleizes to this root
    /// under `ipfs add -r --cid-version=1`. Proves our directory walk + builder
    /// wiring reproduces Kubo's CIDv1 layout.
    const SMALL_DIR_V1: &str = "bafybeic7au5c3eydqdymxffpwtuahdr3saj2dxptw34y6uvkyoybxpuhte";

    #[test]
    fn small_dir_matches_kubo() {
        let dir = std::env::temp_dir().join(format!("dotkit-merkle-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"a").unwrap();
        std::fs::write(dir.join("b.txt"), b"bb").unwrap();

        let m = merkleize_dir(dir.to_str().unwrap()).unwrap();
        assert_eq!(m.root.to_string(), SMALL_DIR_V1);
        assert_eq!(m.root.codec(), DAG_PB);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Captured live from `ipfs add --only-hash -Q -r --hidden --cid-version=1
    /// --raw-leaves` (kubo 0.40.1) for a realistic build dir: an `index.html`, a
    /// nested `assets/big.bin` larger than one 256 KiB chunk (so the file itself
    /// becomes a balanced dag-pb tree), and a top-level hidden file. Exercises
    /// nesting, multi-chunk files, and `--hidden` inclusion together.
    const NESTED_MULTICHUNK_V1: &str = "bafybeibqpezse4fw2okd7w2dlbkjsjj5fkyno74lhc5wu4ogmjveh5bc4a";

    #[test]
    fn nested_multichunk_dir_matches_kubo() {
        let dir = std::env::temp_dir().join(format!(
            "dotkit-merkle-nested-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(dir.join("index.html"), b"hello world\n").unwrap();
        std::fs::write(dir.join("assets").join("big.bin"), vec![b'a'; 700_000]).unwrap();
        std::fs::write(dir.join(".hidden"), b"x").unwrap();

        let m = merkleize_dir(dir.to_str().unwrap()).unwrap();
        assert_eq!(m.root.to_string(), NESTED_MULTICHUNK_V1);
        assert_eq!(m.root.codec(), DAG_PB);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// Live parity check against a real build dir: set `DOTKIT_COMPARE_DIR` to a
    /// directory and this asserts our native root equals `ipfs add -r --hidden
    /// --cid-version=1 --raw-leaves`. Ignored by default (needs `ipfs` on PATH):
    ///   DOTKIT_COMPARE_DIR=../dotshare/dist cargo test -- --ignored compare_env
    #[test]
    #[ignore]
    fn compare_env_dir_with_kubo() {
        let Ok(dir) = std::env::var("DOTKIT_COMPARE_DIR") else {
            eprintln!("set DOTKIT_COMPARE_DIR to run this comparison");
            return;
        };
        let kubo = std::process::Command::new("ipfs")
            .args([
                "add",
                "--only-hash",
                "-Q",
                "-r",
                "--hidden",
                "--cid-version=1",
                "--raw-leaves",
                &dir,
            ])
            .output()
            .expect("run ipfs add");
        assert!(
            kubo.status.success(),
            "ipfs add failed: {}",
            String::from_utf8_lossy(&kubo.stderr)
        );
        let kubo_root = String::from_utf8(kubo.stdout).unwrap().trim().to_string();

        let m = merkleize_dir(&dir).unwrap();
        assert_eq!(
            m.root.to_string(),
            kubo_root,
            "native vs kubo mismatch for {dir} ({} blocks)",
            m.blocks.len()
        );
        eprintln!(
            "native == kubo: {kubo_root} ({} blocks) for {dir}",
            m.blocks.len()
        );
    }
}
