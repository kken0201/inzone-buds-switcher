//! FF04:0001 コレクションのレポートの解析と問い合わせの組み立て（純粋関数。docs/protocol.md）

const REPORT_ID: u8 = 0x02;
/// 受信レポート `[2..4]`
const RX_MAGIC: [u8; 2] = [0x04, 0xFF];
/// `[6..8]`。受信・送信とも共通
const SIGNATURE: [u8; 2] = [0x96, 0xC3];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Earbuds {
    pub left: bool,
    pub right: bool,
}

impl Earbuds {
    pub fn any(&self) -> bool {
        self.left || self.right
    }
}

/// 各値は 0–100（%）。未接続は None
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    pub left: Option<u8>,
    pub right: Option<u8>,
    pub case: Option<u8>,
}

impl Battery {
    /// 左右の値が取れているかどうかで接続を判定する（`0xFF` は未接続）
    pub fn earbuds(&self) -> Earbuds {
        Earbuds { left: self.left.is_some(), right: self.right.is_some() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Report {
    /// `12 01`。自発送信のみ
    Status(Earbuds),
    /// `14 02`
    Link(Earbuds),
    /// `14 04`
    Battery(Battery),
    Other { kind: u8, sub: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Spontaneous,
    /// 問い合わせへの応答。`seq` は要求と同じ値
    Response { seq: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parsed {
    pub report: Report,
    pub origin: Origin,
}

fn pct(v: u8) -> Option<u8> {
    (v <= 100).then_some(v)
}

/// 入力レポート（レポート ID 込み、末尾の 0 埋めがあってもよい）を解析する。
/// ヘッダ・長さ・チェックサムが合わないものは None
pub fn parse(buf: &[u8]) -> Option<Parsed> {
    if buf.len() < 13 || buf[0] != REPORT_ID || buf[2..4] != RX_MAGIC || buf[6..8] != SIGNATURE {
        return None;
    }
    // チェックサムの位置は [1] + 1（状態 0x0E → [15]、バッテリー 0x12 → [19]）
    let sum_at = buf[1] as usize + 1;
    if sum_at < 12 || sum_at >= buf.len() || checksum(&buf[6..sum_at]) != buf[sum_at] {
        return None;
    }
    let body = &buf[..sum_at];
    let origin = match body[10] {
        0xA0 => Origin::Spontaneous,
        0x10 => Origin::Response { seq: body[11] },
        _ => return None,
    };
    let (kind, sub) = (body[8], body[9]);
    let at = |i: usize| body.get(i).copied();
    let report = match (kind, sub) {
        (0x12, 0x01) => Report::Status(Earbuds { left: at(13)? == 1, right: at(14)? == 1 }),
        (0x14, 0x02) => Report::Link(Earbuds { left: at(43)? == 1, right: at(44)? == 1 }),
        (0x14, 0x04) => Report::Battery(Battery {
            left: pct(at(14)?),
            right: pct(at(16)?),
            case: pct(at(18)?),
        }),
        _ => Report::Other { kind, sub },
    };
    Some(Parsed { report, origin })
}

/// `[6]` から直前までの和の下位 8 ビット
fn checksum(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |a, &b| a.wrapping_add(b))
}

/// 問い合わせの出力レポートを作る。`len` は出力レポート長（レポート ID 込み）。
/// `sub` = 0x04 がバッテリー、0x02 が接続状態
pub fn build_query(len: usize, sub: u8, seq: u8) -> Vec<u8> {
    let mut r = vec![0u8; len.max(14)];
    r[..8].copy_from_slice(&[REPORT_ID, 0x0C, 0x01, 0x00, 0xFC, 0x08, SIGNATURE[0], SIGNATURE[1]]);
    r[8] = 0x41;
    r[9] = sub;
    r[10] = 0x01;
    r[11] = seq;
    r[13] = checksum(&r[6..13]);
    r
}

pub fn hex(data: &[u8]) -> String {
    data.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

/// 末尾の 0 を省いて表示を短くする
pub fn trim_zeros(data: &[u8]) -> &[u8] {
    let end = data.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    &data[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(s: &str) -> Vec<u8> {
        s.split_whitespace().map(|x| u8::from_str_radix(x, 16).unwrap()).collect()
    }

    /// チェックサム付きの受信レポートを組み立てる（`fields` は (オフセット, 値)）
    fn frame(kind: u8, sub: u8, origin: u8, seq: u8, sum_at: usize, fields: &[(usize, u8)]) -> Vec<u8> {
        let mut r = vec![0u8; 64];
        r[..8].copy_from_slice(&[0x02, (sum_at - 1) as u8, 0x04, 0xFF, (sum_at - 4) as u8, 0, 0x96, 0xC3]);
        r[8] = kind;
        r[9] = sub;
        r[10] = origin;
        r[11] = seq;
        for &(i, v) in fields {
            r[i] = v;
        }
        r[sum_at] = checksum(&r[6..sum_at]);
        r
    }

    const STATUS_BOTH: &str = "02 0E 04 FF 0B 00 96 C3 12 01 A0 00 00 01 01 0E";
    const BATTERY: &str = "02 12 04 FF 0F 00 96 C3 14 04 10 01 00 00 63 00 64 FF 64 AC";

    #[test]
    fn status_both_connected() {
        let p = parse(&bytes(STATUS_BOTH)).unwrap();
        assert_eq!(p.report, Report::Status(Earbuds { left: true, right: true }));
        assert_eq!(p.origin, Origin::Spontaneous);
    }

    #[test]
    fn status_with_zero_padding_and_one_side() {
        let mut b = frame(0x12, 0x01, 0xA0, 0, 15, &[(13, 1)]);
        b.truncate(64);
        let p = parse(&b).unwrap();
        assert_eq!(p.report, Report::Status(Earbuds { left: true, right: false }));
        assert!(!Earbuds { left: false, right: false }.any());
    }

    #[test]
    fn battery_response() {
        let p = parse(&bytes(BATTERY)).unwrap();
        assert_eq!(p.origin, Origin::Response { seq: 1 });
        let Report::Battery(b) = p.report else { panic!("{p:?}") };
        assert_eq!(b, Battery { left: Some(99), right: Some(100), case: Some(100) });
        assert_eq!(b.earbuds(), Earbuds { left: true, right: true });
    }

    #[test]
    fn battery_disconnected_side_is_none() {
        let b = frame(0x14, 0x04, 0xA0, 0, 19, &[(14, 0xFF), (16, 50), (18, 80)]);
        let Report::Battery(b) = parse(&b).unwrap().report else { panic!() };
        assert_eq!(b, Battery { left: None, right: Some(50), case: Some(80) });
        assert_eq!(b.earbuds(), Earbuds { left: false, right: true });
    }

    #[test]
    fn link_report() {
        let b = frame(0x14, 0x02, 0x10, 7, 45, &[(43, 1), (44, 0xFF)]);
        let p = parse(&b).unwrap();
        assert_eq!(p.report, Report::Link(Earbuds { left: true, right: false }));
        assert_eq!(p.origin, Origin::Response { seq: 7 });
    }

    #[test]
    fn unknown_report_is_other() {
        let b = frame(0x14, 0x41, 0xA0, 0, 17, &[(13, 1), (14, 0x14)]);
        assert_eq!(parse(&b).unwrap().report, Report::Other { kind: 0x14, sub: 0x41 });
    }

    #[test]
    fn rejects_malformed() {
        let good = bytes(STATUS_BOTH);
        let mut bad_sum = good.clone();
        bad_sum[13] = 0;
        assert!(parse(&bad_sum).is_none());
        let mut bad_id = good.clone();
        bad_id[0] = 0x03;
        assert!(parse(&bad_id).is_none());
        let mut bad_sig = good.clone();
        bad_sig[6] = 0;
        assert!(parse(&bad_sig).is_none());
        assert!(parse(&good[..15]).is_none());
        assert!(parse(&[]).is_none());
        // 本体が足りない（バッテリーの長さで状態を名乗る）
        assert!(parse(&frame(0x14, 0x04, 0xA0, 0, 15, &[])).is_none());
        // 自発送信でも応答でもない
        assert!(parse(&frame(0x12, 0x01, 0x55, 0, 15, &[])).is_none());
    }

    #[test]
    fn out_of_range_percentage_is_none() {
        let b = frame(0x14, 0x04, 0xA0, 0, 19, &[(14, 101), (16, 100), (18, 0)]);
        let Report::Battery(b) = parse(&b).unwrap().report else { panic!() };
        assert_eq!(b, Battery { left: None, right: Some(100), case: Some(0) });
    }

    #[test]
    fn query_matches_documented_format() {
        let q = build_query(64, 0x04, 0x05);
        assert_eq!(q.len(), 64);
        assert_eq!(&q[..14], &bytes("02 0C 01 00 FC 08 96 C3 41 04 01 05 00 A4")[..]);
        assert_eq!(q[13], 0x9B + 0x04 + 0x05);
        assert!(q[14..].iter().all(|&b| b == 0));
        assert_eq!(build_query(0, 2, 1).len(), 14);
    }
}
