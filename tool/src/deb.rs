//! deb 解包（ar + xz + tar）与 ELF 解析/补丁（DT_NEEDED / SONAME / RUNPATH）。
//! 用纯 Rust 重写原 payload/payload_tools.py + patch_runpath.py + check_runpath.py。

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};

pub const TERMUX_REPO: &str = "https://packages.termux.dev/apt/termux-main";
pub const TERMUX_INDEX: &str =
    "https://packages.termux.dev/apt/termux-main/dists/stable/main/binary-aarch64/Packages";

/// Termux Packages 索引里的一条记录。
#[derive(Debug, Clone)]
pub struct PkgInfo {
    pub version: String,
    pub filename: String,
    pub size: u64,
}

/// 构造 HTTP agent：从环境变量读取代理（沙箱走 HTTPS_PROXY 出网）。
fn http_agent(timeout_secs: u64) -> ureq::Agent {
    let mut b = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .timeout_read(std::time::Duration::from_secs(120))
        .timeout_connect(std::time::Duration::from_secs(30));
    for var in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() {
                if let Ok(proxy) = ureq::Proxy::new(&v) {
                    b = b.proxy(proxy);
                    break;
                }
            }
        }
    }
    b.build()
}

/// 下载二进制 Packages 索引，返回原始文本。
pub fn fetch_index() -> Result<String, String> {
    let agent = http_agent(30);
    let body = agent
        .get(TERMUX_INDEX)
        .set("User-Agent", "dsh-tool/3.0")
        .call()
        .map_err(|e| format!("fetch index: {e}"))?
        .into_string()
        .map_err(|e| format!("read index: {e}"))?;
    Ok(body)
}

/// 解析 Packages 索引（RFC822 风格段落），返回 包名 -> 记录。
pub fn parse_index(text: &str) -> HashMap<String, PkgInfo> {
    let mut out = HashMap::new();
    for raw in text.split("\n\n") {
        let mut pkg: Option<String> = None;
        let mut version = String::new();
        let mut filename = String::new();
        let mut size = 0u64;
        for line in raw.lines() {
            let line = line.trim_end_matches('\r');
            if let Some(v) = line.strip_prefix("Package: ") {
                pkg = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("Version: ") {
                version = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("Filename: ") {
                filename = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("Size: ") {
                size = v.trim().parse().unwrap_or(0);
            }
        }
        if let Some(name) = pkg {
            if !filename.is_empty() {
                out.insert(name, PkgInfo { version, filename, size });
            }
        }
    }
    out
}

/// 下载一个文件到本地。
pub fn download(url: &str, dest: &std::path::Path) -> Result<(), String> {
    let agent = http_agent(120);
    let mut resp = agent
        .get(url)
        .set("User-Agent", "dsh-tool/3.0")
        .call()
        .map_err(|e| format!("GET {url}: {e}"))?;
    let mut out = std::fs::File::create(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    std::io::copy(&mut resp.into_reader(), &mut out).map_err(|e| format!("write {}: {e}", dest.display()))?;
    Ok(())
}

/// 从 ar 归档中按成员名取出数据（成员名带尾随 '/'，如 "data.tar.xz/"）。
pub fn ar_extract(data: &[u8], name: &str) -> Option<Vec<u8>> {
    if data.len() < 8 || &data[..8] != b"!<arch>\n" {
        return None;
    }
    let mut i = 8usize;
    while i + 60 <= data.len() {
        let hdr = &data[i..i + 60];
        let n = String::from_utf8_lossy(&hdr[..16]).trim_matches(' ').trim_matches('/').to_string();
        let sz: usize = String::from_utf8_lossy(&hdr[48..58]).trim().parse().unwrap_or(0);
        let body_start = i + 60;
        let body_end = (body_start + sz).min(data.len());
        if n == name.trim_matches('/') {
            return Some(data[body_start..body_end].to_vec());
        }
        i = body_start + sz + (sz % 2);
    }
    None
}

/// 解压 deb 的 data.tar.xz 得到 tar 字节流。
pub fn deb_data_tar_bytes(deb: &[u8]) -> Result<Vec<u8>, String> {
    let xz = ar_extract(deb, "data.tar.xz")
        .ok_or_else(|| "no data.tar.xz in deb".to_string())?;
    let mut input = BufReader::new(xz.as_slice());
    let mut out = Vec::new();
    lzma_rs::xz_decompress(&mut input, &mut out).map_err(|e| format!("xz: {e}"))?;
    Ok(out)
}

/// 遍历 tar 成员（目录跳过），返回 (tar 内部路径, 内容)。
pub fn tar_files(data: &[u8]) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut out = Vec::new();
    let mut ar = tar::Archive::new(data);
    for entry in ar.entries().map_err(|e| format!("tar: {e}"))? {
        let mut e = entry.map_err(|e| format!("tar entry: {e}"))?;
        if e.header().entry_type().is_dir() {
            continue;
        }
        let name = e.path().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        let mut buf = Vec::new();
        e.read_to_end(&mut buf).map_err(|e| format!("tar read {name}: {e}"))?;
        out.push((name, buf));
    }
    Ok(out)
}

// ---------- ELF64/32 解析 ----------

struct Elf<'a> {
    data: &'a [u8],
    is64: bool,
}

impl<'a> Elf<'a> {
    fn new(data: &'a [u8]) -> Option<Self> {
        if data.len() < 64 || &data[..4] != b"\x7fELF" {
            return None;
        }
        let is64 = data[4] == 2;
        Some(Elf { data, is64 })
    }
    fn e_phoff(&self) -> u64 {
        if self.is64 { rd_u64(self.data, 32) } else { rd_u32(self.data, 28) as u64 }
    }
    fn e_phentsize(&self) -> usize {
        let v = if self.is64 { rd_u16(self.data, 54) } else { rd_u16(self.data, 42) };
        v as usize
    }
    fn e_phnum(&self) -> usize {
        let v = if self.is64 { rd_u16(self.data, 56) } else { rd_u16(self.data, 44) };
        v as usize
    }
    /// 找到 PT_DYNAMIC 段（type==2）在文件中的偏移。
    fn dynamic_off(&self) -> Option<u64> {
        for k in 0..self.e_phnum() {
            let off = self.e_phoff() + (k as u64) * (self.e_phentsize() as u64);
            let ptype = rd_u32(self.data, off as usize);
            if ptype == 2 {
                return Some(if self.is64 {
                    rd_u64(self.data, (off + 8) as usize)
                } else {
                    rd_u32(self.data, (off + 8) as usize) as u64
                });
            }
        }
        None
    }
    fn entsize(&self) -> usize { if self.is64 { 16 } else { 8 } }
    /// 遍历动态节，返回 (tag, val) 列表。
    fn dyn_tags(&self) -> Option<Vec<(u64, u64)>> {
        let dynoff = self.dynamic_off()? as usize;
        let mut tags = Vec::new();
        let mut j = 0usize;
        loop {
            let off = dynoff + j * self.entsize();
            if off + self.entsize() > self.data.len() {
                break;
            }
            let (tag, val) = if self.is64 {
                (rd_u64(self.data, off), rd_u64(self.data, off + 8))
            } else {
                (rd_u32(self.data, off) as u64, rd_u32(self.data, off + 4) as u64)
            };
            if tag == 0 {
                break;
            }
            tags.push((tag, val));
            j += 1;
        }
        Some(tags)
    }
    /// 遍历程序头，返回 (phony_type, p_offset, p_vaddr, p_filesz)。
    fn phdrs(&self) -> Vec<(u32, u64, u64, u64)> {
        let mut v = Vec::new();
        for k in 0..self.e_phnum() {
            let off = self.e_phoff() + (k as u64) * (self.e_phentsize() as u64);
            let ptype = rd_u32(self.data, off as usize);
            let (foff, addr, fsz) = if self.is64 {
                (rd_u64(self.data, (off + 8) as usize),
                 rd_u64(self.data, (off + 16) as usize),
                 rd_u64(self.data, (off + 32) as usize))
            } else {
                (rd_u32(self.data, (off + 4) as usize) as u64,
                 rd_u32(self.data, (off + 8) as usize) as u64,
                 rd_u32(self.data, (off + 16) as usize) as u64)
            };
            v.push((ptype, foff, addr, fsz));
        }
        v
    }
    /// 把动态节返回的虚拟地址（d_ptr）转换为文件偏移。
    /// loadeadd 库没有通用偏移，但动态字符串几乎总在某个 PT_LOAD 的文件映射内。
    fn vaddr_to_off(&self, addr: u64) -> Option<u64> {
        for (ptype, foff, vaddr, fsz) in self.phdrs() {
            if ptype != 1 {
                continue;
            }
            if addr >= vaddr && addr < vaddr + fsz {
                return Some(foff + (addr - vaddr));
            }
        }
        // 回退：很多 Android .so 直接用文件偏移当作 vaddr（p_vaddr==p_offset）
        if addr < self.data.len() as u64 {
            Some(addr)
        } else {
            None
        }
    }
    fn str_at(&self, addr: u64) -> Option<String> {
        let off = self.vaddr_to_off(addr)? as usize;
        if off >= self.data.len() {
            return None;
        }
        let end = self.data[off..].iter().position(|&b| b == 0).unwrap_or(0);
        let s = String::from_utf8_lossy(&self.data[off..off + end]).to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
    /// DT_STRTAB 的（虚拟地址）值转成文件偏移。
    fn strtab_off(&self) -> Option<u64> {
        let tags = self.dyn_tags()?;
        let strtab_vaddr = tags.iter().find(|(t, _)| *t == 5).map(|(_, v)| *v)?;
        self.vaddr_to_off(strtab_vaddr)
    }
    /// 从 strtab 相对偏移处读以 NUL 结尾的字符串。
    /// 动态节里 DT_NEEDED/SONAME/RUNPATH 等字符串类标签的值都是 strtab 偏移。
    fn dyn_str_at(&self, strtab_off: u64, rel: u64) -> Option<String> {
        let start = (strtab_off + rel) as usize;
        if start >= self.data.len() {
            return None;
        }
        let end = self.data[start..].iter().position(|&b| b == 0).unwrap_or(0);
        let s = String::from_utf8_lossy(&self.data[start..start + end]).to_string();
        if s.is_empty() {
            None
        } else {
            Some(s)
        }
    }
    /// DT_NEEDED（tag=1）列表。
    pub fn needed(&self) -> Vec<String> {
        let mut out = Vec::new();
        let strtab = match self.strtab_off() {
            Some(s) => s,
            None => return out,
        };
        if let Some(tags) = self.dyn_tags() {
            for (tag, val) in tags {
                if tag == 1 {
                    if let Some(s) = self.dyn_str_at(strtab, val) {
                        out.push(s);
                    }
                }
            }
        }
        out
    }
    /// DT_SONAME（tag=14）。
    pub fn soname(&self) -> Option<String> {
        let strtab = self.strtab_off()?;
        self.dyn_tags()?.into_iter().find(|(t, _)| *t == 14).and_then(|(_, v)| self.dyn_str_at(strtab, v))
    }
    /// 返回 (strtab 文件偏移, RUNPATH/RPATH 动态项列表)。
    fn runpath_info(&self) -> Option<(u64, Vec<(u64, u64)>)> {
        let strtab = self.strtab_off()?;
        let tags = self.dyn_tags()?;
        let rps: Vec<_> = tags.iter().filter(|(t, _)| *t == 15 || *t == 29).map(|(t, v)| (*t, *v)).collect();
        Some((strtab, rps))
    }
    /// 检查 DT_NEEDED 是否都能在 lib 目录里找到。
    pub fn check_needed(&self) -> Vec<String> { self.needed() }
}

fn rd_u16(d: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([d.get(off).copied().unwrap_or(0), d.get(off + 1).copied().unwrap_or(0)])
}
fn rd_u32(d: &[u8], off: usize) -> u32 {
    let mut b = [0u8; 4];
    for (i, x) in b.iter_mut().enumerate() {
        *x = d.get(off + i).copied().unwrap_or(0);
    }
    u32::from_le_bytes(b)
}
fn rd_u64(d: &[u8], off: usize) -> u64 {
    let mut b = [0u8; 8];
    for (i, x) in b.iter_mut().enumerate() {
        *x = d.get(off + i).copied().unwrap_or(0);
    }
    u64::from_le_bytes(b)
}

/// 取 ELF 的 DT_NEEDED 列表。
pub fn elf_needed(data: &[u8]) -> Vec<String> {
    Elf::new(data).map(|e| e.needed()).unwrap_or_default()
}

/// 取 ELF 的 SONAME。
pub fn elf_soname(data: &[u8]) -> Option<String> {
    Elf::new(data).and_then(|e| e.soname())
}

/// 把文件里的 RUNPATH/RPATH 改写为 new_rpath（原地修改）。
/// 返回成功补丁的个数。
pub fn patch_runpath_file(path: &std::path::Path, new_rpath: &str) -> Result<usize, String> {
    let data = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let elf = Elf::new(&data).ok_or_else(|| format!("not ELF: {}", path.display()))?;
    let (strtab, rps) = elf
        .runpath_info()
        .ok_or_else(|| format!("no strtab: {}", path.display()))?;
    if rps.is_empty() {
        return Ok(0);
    }
    // 收集所有要写的 (offset, 旧串)
    let mut patches: Vec<(u64, String)> = Vec::new();
    for (_, val) in &rps {
        let start = strtab + val;
        let end = data[start as usize..]
            .iter()
            .position(|&b| b == 0)
            .map(|p| start + p as u64)
            .unwrap_or(data.len() as u64);
        let old = String::from_utf8_lossy(&data[start as usize..end as usize]).to_string();
        if new_rpath.len() > old.len() {
            return Err(format!(
                "new rpath too long for {}: {:?} > {:?}",
                path.display(),
                new_rpath,
                old
            ));
        }
        patches.push((start, old));
    }
    let mut out = data.clone();
    for (start, old) in &patches {
        let start = *start as usize;
        out[start..start + new_rpath.len()].copy_from_slice(new_rpath.as_bytes());
        for i in (start + new_rpath.len())..(start + old.len()) {
            out[i] = 0;
        }
    }
    std::fs::write(path, &out).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(patches.len())
}

/// 打印文件的 RUNPATH/RPATH 值（校验用）。
pub fn check_runpath(path: &std::path::Path) -> Result<Vec<String>, String> {
    let data = std::fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let elf = Elf::new(&data).ok_or_else(|| format!("not ELF: {}", path.display()))?;
    let mut out = Vec::new();
    if let Some((strtab, rps)) = elf.runpath_info() {
        for (_, val) in rps {
            let start = strtab + val;
            let end = data[start as usize..]
                .iter()
                .position(|&b| b == 0)
                .map(|p| start + p as u64)
                .unwrap_or(data.len() as u64);
            out.push(String::from_utf8_lossy(&data[start as usize..end as usize]).to_string());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod dbg_libcpp {
    use super::*;
    #[test]
    fn inspect_libcpp() {
        let deb = std::fs::read("/workspace/dsh-mobile-rust/debs/libc++.deb").unwrap();
        let tar = deb_data_tar_bytes(&deb).unwrap();
        println!("tar len {}", tar.len());
        let mut ar = tar::Archive::new(tar.as_slice());
        let mut count = 0;
        for entry in ar.entries().unwrap() {
            let e = entry.unwrap();
            let p = e.path().unwrap().to_string_lossy().to_string();
            if !p.contains("usr/lib") {
                continue;
            }
            println!("{} [sz={}]", p, e.header().size().unwrap());
            count += 1;
            if count > 8 { break; }
        }
    }
}

#[cfg(test)]
mod dbg_scan {
    use super::*;
    #[test]
    fn scan_all() {
        for name in ["nodejs","bash","ripgrep","libandroid-support","libiconv","readline","ncurses","pcre2","libc++","openssl","c-ares","libicu","libsqlite","zlib","libffi","ca-certificates"] {
            let p = format!("/workspace/dsh-mobile-rust/debs/{name}.deb");
            let Ok(deb) = std::fs::read(&p) else { println!("{name}: MISSING"); continue };
            let Ok(tar) = deb_data_tar_bytes(&deb) else { println!("{name}: xz fail"); continue };
            match tar_files(&tar) {
                Ok(files) => {
                    let weird: Vec<String> = files.iter().map(|(n,_)| n.clone()).filter(|n| n.len() > 100 || n.contains('\'') || n == "" || n.contains("base12")).collect();
                    if weird.is_empty() { println!("{name}: {} entries ok", files.len()); }
                    else { println!("{name}: {} entries WEIRD={} first={:?}", files.len(), weird.len(), weird.first()); }
                }
                Err(e) => println!("{name}: tar fail {e}"),
            }
        }
    }
}
