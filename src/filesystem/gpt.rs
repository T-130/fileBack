//! 最小、只读的 GPT 解析与有界分区视图。

use anyhow::{Context, Result, bail};
use std::io::{Read, Seek, SeekFrom};

/// GPT 文本 GUID 为 7C3457EF-0000-11AA-AA11-00306543ECAC；前三个字段按小端落盘。
pub(super) const APFS_PARTITION_TYPE: [u8; 16] = [
    0xef, 0x57, 0x34, 0x7c, 0x00, 0x00, 0xaa, 0x11, 0xaa, 0x11, 0x00, 0x30, 0x65, 0x43, 0xec, 0xac,
];
const GPT_HEADER_SIZE: usize = 92;
const MAX_PARTITION_ENTRIES: u32 = 16_384;
const MAX_ENTRY_SIZE: usize = 4096;
const MAX_ENTRY_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Partition {
    pub(crate) start: u64,
    pub(crate) end: u64,
    pub(crate) len: u64,
}

pub(crate) fn apfs_partitions<R: Read + Seek>(
    reader: &mut R,
    disk_size: u64,
) -> Result<Vec<Partition>> {
    let mut last_error = None;
    for sector_size in [512_u64, 4096_u64] {
        match parse_gpt(reader, disk_size, sector_size) {
            Ok(partitions) => return Ok(partitions),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("磁盘容量无效")))
        .context("未找到有效的主 GPT 头")
}

fn parse_gpt<R: Read + Seek>(
    reader: &mut R,
    disk_size: u64,
    sector_size: u64,
) -> Result<Vec<Partition>> {
    let header_offset = sector_size;
    let header_end = header_offset
        .checked_add(sector_size)
        .context("GPT 头偏移溢出")?;
    if disk_size < header_end {
        bail!("磁盘小于一个 GPT 头");
    }
    let sector_len = usize::try_from(sector_size).context("GPT 扇区大小无效")?;
    let sector = read_exact(reader, header_offset, sector_len)?;
    if sector.get(..8) != Some(b"EFI PART") {
        bail!("GPT 签名不匹配");
    }
    let header_size = le_u32(&sector, 12) as usize;
    if !(GPT_HEADER_SIZE..=sector_len).contains(&header_size) {
        bail!("GPT 头长度无效：{header_size}");
    }
    if le_u32(&sector, 20) != 0 || le_u64(&sector, 24) != 1 {
        bail!("GPT 头保留字段或当前 LBA 无效");
    }

    let stored_header_crc = le_u32(&sector, 16);
    let mut checked_header = sector[..header_size].to_vec();
    checked_header[16..20].fill(0);
    if crc32fast::hash(&checked_header) != stored_header_crc {
        bail!("GPT 头 CRC 不匹配");
    }

    let first_usable = le_u64(&sector, 40);
    let last_usable = le_u64(&sector, 48);
    if first_usable > last_usable {
        bail!("GPT 可用 LBA 范围无效");
    }
    let entries_lba = le_u64(&sector, 72);
    let entry_count = le_u32(&sector, 80);
    let entry_size = le_u32(&sector, 84) as usize;
    if entry_count > MAX_PARTITION_ENTRIES
        || !(128..=MAX_ENTRY_SIZE).contains(&entry_size)
        || !entry_size.is_multiple_of(8)
    {
        bail!("GPT 分区条目数量或长度无效");
    }
    let entries_len = (entry_count as usize)
        .checked_mul(entry_size)
        .context("GPT 分区表长度溢出")?;
    if entries_len > MAX_ENTRY_BYTES {
        bail!("GPT 分区表超过 16 MiB 上限");
    }
    let entries_offset = entries_lba
        .checked_mul(sector_size)
        .context("GPT 分区表偏移溢出")?;
    let entries_end = entries_offset
        .checked_add(entries_len as u64)
        .context("GPT 分区表结束位置溢出")?;
    if entries_end > disk_size {
        bail!("GPT 分区表超出磁盘范围");
    }
    let entries = read_exact(reader, entries_offset, entries_len)?;
    if crc32fast::hash(&entries) != le_u32(&sector, 88) {
        bail!("GPT 分区表 CRC 不匹配");
    }

    let mut partitions = Vec::new();
    for index in 0..entry_count as usize {
        let entry = &entries[index * entry_size..(index + 1) * entry_size];
        if entry[..16] != APFS_PARTITION_TYPE {
            continue;
        }
        let first_lba = le_u64(entry, 32);
        let last_lba = le_u64(entry, 40);
        if first_lba < first_usable || first_lba > last_lba || last_lba > last_usable {
            bail!("APFS 分区 {} 的 LBA 范围无效", index + 1);
        }
        let start = first_lba
            .checked_mul(sector_size)
            .context("APFS 分区起始偏移溢出")?;
        let end = last_lba
            .checked_add(1)
            .and_then(|lba| lba.checked_mul(sector_size))
            .context("APFS 分区结束偏移溢出")?;
        if end > disk_size || start >= end {
            bail!("APFS 分区 {} 超出磁盘范围", index + 1);
        }
        partitions.push(Partition {
            start,
            end,
            len: end - start,
        });
    }
    partitions.sort_unstable_by_key(|partition| partition.start);
    if partitions
        .windows(2)
        .any(|pair| pair[0].end > pair[1].start)
    {
        bail!("APFS 容器分区互相重叠");
    }
    Ok(partitions)
}

fn read_exact<R: Read + Seek>(reader: &mut R, offset: u64, len: usize) -> Result<Vec<u8>> {
    let mut data = vec![0_u8; len];
    reader.seek(SeekFrom::Start(offset))?;
    reader.read_exact(&mut data)?;
    Ok(data)
}

fn le_u32(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap_or([0; 4]))
}

fn le_u64(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap_or([0; 8]))
}

/// 将分区偏移 0 映射到容器开头，并阻止解析器越过分区末尾读取。
pub(crate) struct PartitionView<R> {
    inner: R,
    start: u64,
    len: u64,
    position: u64,
}

impl<R> PartitionView<R> {
    pub(crate) fn new(inner: R, partition: &Partition) -> Self {
        Self {
            inner,
            start: partition.start,
            len: partition.len,
            position: 0,
        }
    }
}

impl<R: Read + Seek> Read for PartitionView<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if buffer.is_empty() || self.position == self.len {
            return Ok(0);
        }
        let remaining = self.len - self.position;
        let requested = usize::try_from(remaining)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let absolute = self.start.checked_add(self.position).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "分区读取偏移溢出")
        })?;
        self.inner.seek(SeekFrom::Start(absolute))?;
        let read = self.inner.read(&mut buffer[..requested])?;
        self.position = self.position.checked_add(read as u64).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "分区读取位置溢出")
        })?;
        Ok(read)
    }
}

impl<R: Read + Seek> Seek for PartitionView<R> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        let next = match position {
            SeekFrom::Start(offset) => i128::from(offset),
            SeekFrom::Current(delta) => i128::from(self.position) + i128::from(delta),
            SeekFrom::End(delta) => i128::from(self.len) + i128::from(delta),
        };
        if next < 0 || next > i128::from(self.len) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "不能越过 APFS 分区边界",
            ));
        }
        self.position = next as u64;
        Ok(self.position)
    }
}
