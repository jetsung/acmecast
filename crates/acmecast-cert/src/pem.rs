//! 证书与私钥的 PEM / DER 编解码。
//!
//! PEM 就是「头标记 + Base64(DER) + 尾标记」，本模块只做编解码，
//! 不解释证书语义。语义解析见 [`crate::parse`]。
//!
//! 不引入 PEM 库的原因：需要同时支持编与码两个方向，且要精确控制
//! 换行宽度与尾随换行，自己实现反而比拼接两个库更可控。

use base64::Engine;

use crate::error::{Error, Result};

/// 证书块的 PEM 头标记。
pub const CERT_PEM_HEADER: &str = "-----BEGIN CERTIFICATE-----";
/// 证书块的 PEM 尾标记。
pub const CERT_PEM_FOOTER: &str = "-----END CERTIFICATE-----";

/// PEM 正文每行的字符数。RFC 7468 要求不超过 64。
const PEM_LINE_WIDTH: usize = 64;

/// 从 PEM 文本中提取全部证书块的 DER 字节。
///
/// 支持一个文件包含多张证书（证书链），也容忍 CRLF 与多余空行。
/// 非 `CERTIFICATE` 类型的块（例如私钥）会被忽略。
pub fn pem_to_der_blocks(pem: &str) -> Result<Vec<Vec<u8>>> {
    let mut blocks = Vec::new();
    let mut cursor = pem;

    while let Some(start) = cursor.find(CERT_PEM_HEADER) {
        let after_header = &cursor[start + CERT_PEM_HEADER.len()..];
        let Some(end) = after_header.find(CERT_PEM_FOOTER) else {
            return Err(Error::Pem(format!(
                "证书块缺少结束标记 `{CERT_PEM_FOOTER}`"
            )));
        };

        let body: String = after_header[..end]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();

        let der = base64::engine::general_purpose::STANDARD
            .decode(&body)
            .map_err(|e| Error::Pem(format!("证书块 base64 解码失败: {e}")))?;

        blocks.push(der);
        cursor = &after_header[end + CERT_PEM_FOOTER.len()..];
    }

    Ok(blocks)
}

/// 把 DER 编码的证书编码为 PEM 文本。
///
/// 输出以换行结尾，符合 PEM 文件惯例。
#[must_use]
pub fn der_to_pem(der: &[u8]) -> String {
    let body = base64::engine::general_purpose::STANDARD.encode(der);

    let mut out = String::with_capacity(body.len() + 64);
    out.push_str(CERT_PEM_HEADER);
    out.push('\n');
    for chunk in body.as_bytes().chunks(PEM_LINE_WIDTH) {
        out.push_str(&String::from_utf8_lossy(chunk));
        out.push('\n');
    }
    out.push_str(CERT_PEM_FOOTER);
    out.push('\n');
    out
}

/// 把多张 DER 证书编码为一串 PEM（证书链）。
#[must_use]
pub fn chain_to_pem(chain: &[Vec<u8>]) -> String {
    chain.iter().map(|der| der_to_pem(der)).collect()
}

/// 从 PEM 中取出第一张证书的 DER。
///
/// 私钥与证书混在同一个文件里时也适用——私钥块会被跳过。
pub fn first_der(pem: &str) -> Result<Vec<u8>> {
    pem_to_der_blocks(pem)?
        .into_iter()
        .next()
        .ok_or(Error::NoCertificate)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一段合法的 DER（此处只需能被 base64 往返，无需是证书）。
    const SAMPLE_DER: &[u8] = &[0x30, 0x82, 0x01, 0x0a, 0x02, 0x01, 0x01];

    #[test]
    fn pem_roundtrips_through_der() {
        let pem = der_to_pem(SAMPLE_DER);
        let blocks = pem_to_der_blocks(&pem).unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0], SAMPLE_DER);
    }

    #[test]
    fn pem_has_correct_markers() {
        let pem = der_to_pem(SAMPLE_DER);
        assert!(pem.starts_with(CERT_PEM_HEADER));
        assert!(pem.trim_end().ends_with(CERT_PEM_FOOTER));
        assert!(pem.ends_with('\n'), "PEM 应以换行结尾");
    }

    #[test]
    fn long_der_wraps_at_64_columns() {
        let long_der: Vec<u8> = (0..=200u8).collect();
        let pem = der_to_pem(&long_der);
        for line in pem.lines() {
            if line.starts_with("-----") {
                continue;
            }
            assert!(
                line.len() <= PEM_LINE_WIDTH,
                "PEM 正文行不得超过 {PEM_LINE_WIDTH} 列，实际 {} 列: {line}",
                line.len()
            );
        }
        assert_eq!(pem_to_der_blocks(&pem).unwrap()[0], long_der);
    }

    #[test]
    fn chain_is_parsed_into_all_blocks() {
        let chain = chain_to_pem(&[vec![1, 2, 3], vec![4, 5, 6], vec![7, 8, 9]]);
        let blocks = pem_to_der_blocks(&chain).unwrap();
        assert_eq!(blocks.len(), 3, "链中每张证书都应被取出");
        assert_eq!(blocks[0], vec![1, 2, 3]);
        assert_eq!(blocks[2], vec![7, 8, 9]);
    }

    #[test]
    fn crlf_is_tolerated() {
        let pem = der_to_pem(SAMPLE_DER).replace('\n', "\r\n");
        let blocks = pem_to_der_blocks(&pem).unwrap();
        assert_eq!(blocks[0], SAMPLE_DER);
    }

    #[test]
    fn extra_blank_lines_are_tolerated() {
        let mut pem = String::from("\n\n");
        pem.push_str(&der_to_pem(SAMPLE_DER));
        pem.push_str("\n\n");
        assert_eq!(pem_to_der_blocks(&pem).unwrap()[0], SAMPLE_DER);
    }

    #[test]
    fn private_key_blocks_are_skipped() {
        // 常见的「私钥 + 证书」合并文件。
        let combined = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n{}",
            base64::engine::general_purpose::STANDARD.encode([9u8; 8]),
            der_to_pem(SAMPLE_DER)
        );
        let blocks = pem_to_der_blocks(&combined).unwrap();
        assert_eq!(blocks.len(), 1, "只应取出证书块");
        assert_eq!(blocks[0], SAMPLE_DER);
    }

    #[test]
    fn missing_footer_is_rejected() {
        let broken = format!("{CERT_PEM_HEADER}\nAAAA\n");
        match pem_to_der_blocks(&broken) {
            Ok(_) => panic!("缺少结束标记应报错"),
            Err(err) => assert!(err.to_string().contains("结束标记"), "{err}"),
        }
    }

    #[test]
    fn invalid_base64_is_rejected() {
        let broken = format!("{CERT_PEM_HEADER}\n!!!not-base64!!!\n{CERT_PEM_FOOTER}\n");
        match pem_to_der_blocks(&broken) {
            Ok(_) => panic!("非法 base64 应报错"),
            Err(err) => assert!(err.to_string().contains("base64"), "{err}"),
        }
    }

    #[test]
    fn empty_pem_yields_no_blocks() {
        assert!(pem_to_der_blocks("").unwrap().is_empty());
        assert!(pem_to_der_blocks("no pem here").unwrap().is_empty());
    }

    #[test]
    fn first_der_reports_missing_certificate() {
        match first_der("nothing") {
            Ok(_) => panic!("没有证书块时应报错"),
            Err(err) => assert!(matches!(err, Error::NoCertificate), "{err:?}"),
        }
    }
}
