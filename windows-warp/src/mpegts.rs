//! Minimal MPEG-2 transport stream muxer for a single H.264 elementary stream.
//!
//! Only what a player needs to tune into a live UDP stream: a PAT, a PMT and the
//! video PES packets, with the PCR carried on the video PID.

pub const TS_PACKET_LEN: usize = 188;

const SYNC_BYTE: u8 = 0x47;
const PAT_PID: u16 = 0x0000;
const PMT_PID: u16 = 0x1000;
const VIDEO_PID: u16 = 0x0100;
const PROGRAM_NUMBER: u16 = 1;
const TRANSPORT_STREAM_ID: u16 = 1;
const STREAM_TYPE_H264: u8 = 0x1b;
const PES_STREAM_ID_VIDEO: u8 = 0xe0;

/// How often the PAT and the PMT are repeated, in 90 kHz ticks (100 ms).
const PSI_INTERVAL_90KHZ: u64 = 9_000;
/// How far the presentation times run ahead of the clock reference, in 90 kHz
/// ticks (200 ms). Decoders need the PCR to arrive before the PTS it times.
const PCR_LEAD_90KHZ: u64 = 18_000;

pub struct TsMuxer {
    pat_counter: u8,
    pmt_counter: u8,
    video_counter: u8,
    next_psi_pts: Option<u64>,
}

impl Default for TsMuxer {
    fn default() -> Self {
        Self::new()
    }
}

impl TsMuxer {
    pub fn new() -> Self {
        Self {
            pat_counter: 0,
            pmt_counter: 0,
            video_counter: 0,
            next_psi_pts: None,
        }
    }

    /// Appends the transport packets of one access unit (one coded frame, in
    /// Annex B form) to `out`. `pts_90khz` is its presentation time.
    pub fn push_access_unit(
        &mut self,
        au: &[u8],
        pts_90khz: u64,
        keyframe: bool,
        out: &mut Vec<u8>,
    ) {
        if au.is_empty() {
            return;
        }

        let due = self.next_psi_pts.is_none_or(|next| pts_90khz >= next);
        if keyframe || due {
            self.write_pat(out);
            self.write_pmt(out);
            self.next_psi_pts = Some(pts_90khz + PSI_INTERVAL_90KHZ);
        }

        let mut pes = Vec::with_capacity(au.len() + 14);
        write_pes_header(&mut pes, pts_90khz);
        pes.extend_from_slice(au);

        let pcr = pts_90khz.saturating_sub(PCR_LEAD_90KHZ);
        self.write_pes_packets(&pes, pcr, keyframe, out);
    }

    fn write_pes_packets(&mut self, pes: &[u8], pcr_90khz: u64, keyframe: bool, out: &mut Vec<u8>) {
        let mut offset = 0;
        let mut first = true;

        while offset < pes.len() {
            let mut packet = [0xffu8; TS_PACKET_LEN];
            packet[0] = SYNC_BYTE;
            packet[1] = (u8::from(first) << 6) | (VIDEO_PID >> 8) as u8;
            packet[2] = (VIDEO_PID & 0xff) as u8;

            // The first packet of a key frame carries the clock reference and
            // flags the random access point; the last one is padded with an
            // adaptation field when the remaining payload is short.
            let mut adaptation = if first && keyframe {
                let mut field = vec![0x40]; // random_access_indicator
                field.extend_from_slice(&encode_pcr(pcr_90khz));
                Some(field)
            } else {
                None
            };

            let remaining = pes.len() - offset;
            let payload_capacity = TS_PACKET_LEN - 4 - adaptation_field_len(adaptation.as_deref());
            if remaining < payload_capacity {
                // Stuff the adaptation field so that the packet ends exactly on
                // the end of the access unit.
                let field = adaptation.get_or_insert_with(Vec::new);
                let wanted = TS_PACKET_LEN - 4 - 1 - remaining;
                if wanted > 0 && field.is_empty() {
                    field.push(0x00); // no flags
                }
                field.resize(wanted, 0xff);
            }

            let payload_start = match &adaptation {
                None => {
                    packet[3] = 0x10 | (self.video_counter & 0x0f);
                    4
                }
                Some(field) => {
                    packet[3] = 0x30 | (self.video_counter & 0x0f);
                    packet[4] = field.len() as u8;
                    packet[5..5 + field.len()].copy_from_slice(field);
                    5 + field.len()
                }
            };

            let take = (TS_PACKET_LEN - payload_start).min(pes.len() - offset);
            packet[payload_start..payload_start + take]
                .copy_from_slice(&pes[offset..offset + take]);
            out.extend_from_slice(&packet);

            self.video_counter = self.video_counter.wrapping_add(1);
            offset += take;
            first = false;
        }
    }

    fn write_pat(&mut self, out: &mut Vec<u8>) {
        let mut section = vec![
            0x00, // table_id
            0xb0,
            0x0d, // section_syntax_indicator + section_length (13)
            (TRANSPORT_STREAM_ID >> 8) as u8,
            (TRANSPORT_STREAM_ID & 0xff) as u8,
            0xc1, // version 0, current_next_indicator
            0x00, // section_number
            0x00, // last_section_number
            (PROGRAM_NUMBER >> 8) as u8,
            (PROGRAM_NUMBER & 0xff) as u8,
            0xe0 | (PMT_PID >> 8) as u8,
            (PMT_PID & 0xff) as u8,
        ];
        append_crc32(&mut section);
        write_psi_packet(PAT_PID, &mut self.pat_counter, &section, out);
    }

    fn write_pmt(&mut self, out: &mut Vec<u8>) {
        let mut section = vec![
            0x02, // table_id
            0xb0,
            0x12, // section_syntax_indicator + section_length (18)
            (PROGRAM_NUMBER >> 8) as u8,
            (PROGRAM_NUMBER & 0xff) as u8,
            0xc1, // version 0, current_next_indicator
            0x00, // section_number
            0x00, // last_section_number
            0xe0 | (VIDEO_PID >> 8) as u8,
            (VIDEO_PID & 0xff) as u8, // PCR_PID
            0xf0,
            0x00, // program_info_length
            STREAM_TYPE_H264,
            0xe0 | (VIDEO_PID >> 8) as u8,
            (VIDEO_PID & 0xff) as u8,
            0xf0,
            0x00, // ES_info_length
        ];
        append_crc32(&mut section);
        write_psi_packet(PMT_PID, &mut self.pmt_counter, &section, out);
    }
}

/// Bytes a packet spends on its adaptation field, length prefix included.
fn adaptation_field_len(adaptation: Option<&[u8]>) -> usize {
    adaptation.map_or(0, |field| field.len() + 1)
}

fn write_psi_packet(pid: u16, counter: &mut u8, section: &[u8], out: &mut Vec<u8>) {
    let mut packet = [0xffu8; TS_PACKET_LEN];
    packet[0] = SYNC_BYTE;
    packet[1] = 0x40 | (pid >> 8) as u8;
    packet[2] = (pid & 0xff) as u8;
    packet[3] = 0x10 | (*counter & 0x0f);
    packet[4] = 0x00; // pointer_field
    packet[5..5 + section.len()].copy_from_slice(section);
    out.extend_from_slice(&packet);
    *counter = counter.wrapping_add(1);
}

fn write_pes_header(out: &mut Vec<u8>, pts_90khz: u64) {
    out.extend_from_slice(&[0x00, 0x00, 0x01, PES_STREAM_ID_VIDEO]);
    // Unbounded length: allowed, and required, for video PES packets that do
    // not fit in 65535 bytes.
    out.extend_from_slice(&[0x00, 0x00]);
    out.push(0x84); // '10' marker + data_alignment_indicator
    out.push(0x80); // PTS only
    out.push(0x05); // PES_header_data_length
    out.extend_from_slice(&encode_timestamp(0b0010, pts_90khz));
}

/// 33 bit timestamp interleaved with marker bits, as PTS_DTS fields are coded.
fn encode_timestamp(prefix: u8, value: u64) -> [u8; 5] {
    let value = value & 0x1_ffff_ffff;
    [
        (prefix << 4) | (((value >> 30) & 0x07) as u8) << 1 | 0x01,
        ((value >> 22) & 0xff) as u8,
        ((((value >> 15) & 0x7f) as u8) << 1) | 0x01,
        ((value >> 7) & 0xff) as u8,
        (((value & 0x7f) as u8) << 1) | 0x01,
    ]
}

/// 42 bit program clock reference: a 33 bit 90 kHz base and a 9 bit 27 MHz
/// extension.
fn encode_pcr(pcr_90khz: u64) -> [u8; 6] {
    let base = pcr_90khz & 0x1_ffff_ffff;
    let extension = 0u16;
    [
        ((base >> 25) & 0xff) as u8,
        ((base >> 17) & 0xff) as u8,
        ((base >> 9) & 0xff) as u8,
        ((base >> 1) & 0xff) as u8,
        (((base & 0x01) as u8) << 7) | 0x7e | ((extension >> 8) as u8 & 0x01),
        (extension & 0xff) as u8,
    ]
}

fn append_crc32(section: &mut Vec<u8>) {
    let crc = crc32_mpeg2(section);
    section.extend_from_slice(&crc.to_be_bytes());
}

fn crc32_mpeg2(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in data {
        crc ^= (*byte as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(packet: &[u8]) -> u16 {
        (((packet[1] & 0x1f) as u16) << 8) | packet[2] as u16
    }

    #[test]
    fn packets_are_aligned_and_tagged() {
        let mut muxer = TsMuxer::new();
        let mut out = Vec::new();
        let au: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        muxer.push_access_unit(&au, 90_000, true, &mut out);

        assert_eq!(out.len() % TS_PACKET_LEN, 0);
        let packets: Vec<&[u8]> = out.chunks(TS_PACKET_LEN).collect();
        assert!(packets.iter().all(|packet| packet[0] == SYNC_BYTE));
        assert_eq!(pid(packets[0]), PAT_PID);
        assert_eq!(pid(packets[1]), PMT_PID);
        assert!(packets[2..].iter().all(|packet| pid(packet) == VIDEO_PID));
        assert_eq!(packets[2][1] & 0x40, 0x40, "payload unit start indicator");
    }

    #[test]
    fn continuity_counter_increments_per_pid() {
        let mut muxer = TsMuxer::new();
        let mut out = Vec::new();
        for frame in 0..4 {
            muxer.push_access_unit(&[0u8; 400], frame * 1500, frame == 0, &mut out);
        }

        let mut expected = 0u8;
        for packet in out.chunks(TS_PACKET_LEN) {
            if pid(packet) != VIDEO_PID {
                continue;
            }
            assert_eq!(packet[3] & 0x0f, expected);
            expected = (expected + 1) & 0x0f;
        }
    }

    #[test]
    fn short_access_unit_is_padded_into_one_packet() {
        let mut muxer = TsMuxer::new();
        let mut out = Vec::new();
        muxer.push_access_unit(&[0u8; 8], 0, false, &mut out);

        let packets: Vec<&[u8]> = out.chunks(TS_PACKET_LEN).collect();
        assert_eq!(packets.len(), 3, "PAT, PMT and one video packet");
        let video = packets[2];
        assert_eq!(video[3] & 0x30, 0x30, "adaptation field and payload");
        assert_eq!(video[4] as usize, TS_PACKET_LEN - 5 - 14 - 8);
    }

    #[test]
    fn timestamps_round_trip() {
        let pts = 0x1_2345_6789u64;
        let encoded = encode_timestamp(0b0010, pts);
        let decoded = (((encoded[0] >> 1) & 0x07) as u64) << 30
            | (encoded[1] as u64) << 22
            | ((encoded[2] >> 1) as u64) << 15
            | (encoded[3] as u64) << 7
            | (encoded[4] >> 1) as u64;
        assert_eq!(decoded, pts);
    }

    #[test]
    fn crc_matches_reference_vector() {
        // Table based reference value for the standard "123456789" vector.
        assert_eq!(crc32_mpeg2(b"123456789"), 0x0376_e6e7);
    }
}
