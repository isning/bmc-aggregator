use crate::bmc::feature::msd::{MsdDigestAlgorithm, MsdMediaDigest};

pub async fn calculate_digests(
    path: &std::path::Path,
) -> Result<Vec<MsdMediaDigest>, std::io::Error> {
    let path = path.to_owned();
    crate::bmc::cpu::run_batch(move || calculate_digests_blocking(&path))
        .await
        .map_err(std::io::Error::other)?
}
fn calculate_digests_blocking(
    path: &std::path::Path,
) -> Result<Vec<MsdMediaDigest>, std::io::Error> {
    use md5::{Digest, Md5};
    use sha1::Sha1;
    use sha2::{Sha256, Sha512};
    use std::io::Read;

    let mut file = std::fs::File::open(path)?;
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut md5 = Md5::new();
    let mut sha1 = Sha1::new();
    let mut sha256 = Sha256::new();
    let mut sha512 = Sha512::new();
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        let bytes = &buffer[..count];
        md5.update(bytes);
        sha1.update(bytes);
        sha256.update(bytes);
        sha512.update(bytes);
    }
    Ok(vec![
        MsdMediaDigest {
            algorithm: MsdDigestAlgorithm::Md5,
            value: format!("{:x}", md5.finalize()),
        },
        MsdMediaDigest {
            algorithm: MsdDigestAlgorithm::Sha1,
            value: format!("{:x}", sha1.finalize()),
        },
        MsdMediaDigest {
            algorithm: MsdDigestAlgorithm::Sha256,
            value: format!("{:x}", sha256.finalize()),
        },
        MsdMediaDigest {
            algorithm: MsdDigestAlgorithm::Sha512,
            value: format!("{:x}", sha512.finalize()),
        },
    ])
}
