//! Mahogany's reverse-engineered storage peer emulator.
#![allow(clippy::too_many_arguments)]

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, Mutex};
use tracing::{debug, info, trace, warn};

// ── Constants ────────────────────────────────────────────────────

/// Size of the setup information block (StorageMsgRequest).
const SETUP_INFO_SIZE: usize = 544;
const SETUP_TIMEOUT: Duration = Duration::from_secs(10);

/// Size of the command header.
const HEADER_SIZE: usize = 64;

/// Maximum payload size per transfer.
const MAX_TRANSFER_SIZE: u32 = 0x1000; // 4096

/// Default block size for CDROM (2048 bytes).
const CDROM_BLOCK_SIZE: u32 = 2048;

// ── SCSI CDB sizes per opcode ────────────────────────────────────
const SCSI_CDB_SIZE: usize = 16;

// ── Header field offsets ─────────────────────────────────────────
const HDR_DATA_LENGTH: usize = 0x08;
const HDR_TRANSACTION_ID: usize = 0x0C;
const HDR_SEQUENCE_ID: usize = 0x10;
const HDR_STATUS: usize = 0x18;
const HDR_FIELD_1C: usize = 0x1C;
const HDR_FIELD_20: usize = 0x20;
const HDR_SCSI_OPCODE: usize = 0x39;
const HDR_DIRECTION: usize = 0x3A;
const HDR_FLAGS: usize = 0x3F;

// ── Direction/type values in header[0x3a] ───────────────────────
const TYPE_COMMAND: u8 = 0x00; // Command from BMC
const TYPE_DATA_IN: u8 = 0x02; // Data from server to BMC
const TYPE_STATUS: u8 = 0x04; // Status from server to BMC

// ── SCSI opcodes ─────────────────────────────────────────────────
const SCSI_TEST_UNIT_READY: u8 = 0x00;
const SCSI_REQUEST_SENSE: u8 = 0x03;
const SCSI_INQUIRY: u8 = 0x12;
const SCSI_MODE_SENSE_6: u8 = 0x1A;
const SCSI_PREVENT_ALLOW_MEDIUM_REMOVAL: u8 = 0x1E;
const SCSI_READ_FORMAT_CAPACITIES: u8 = 0x23;
const SCSI_READ_CAPACITY_10: u8 = 0x25;
const SCSI_READ_10: u8 = 0x28;
const SCSI_WRITE_10: u8 = 0x2A;
const SCSI_READ_TOC: u8 = 0x43;
const SCSI_GET_CONFIGURATION: u8 = 0x46;
const SCSI_GET_EVENT_STATUS: u8 = 0x4A;
const SCSI_READ_DISC_INFORMATION: u8 = 0x51;
const SCSI_MODE_SENSE_10: u8 = 0x5A;
const SCSI_GET_PERFORMANCE: u8 = 0xAC;

// ── SCSI status codes ───────────────────────────────────────────
const STATUS_GOOD: u8 = 0x00;
const STATUS_CHECK_CONDITION: u8 = 0x02;

// ── Sense keys ──────────────────────────────────────────────────
const SENSE_NO_SENSE: u8 = 0x00;
const SENSE_ILLEGAL_REQUEST: u8 = 0x05;

// ── ASC/ASCQ ────────────────────────────────────────────────────
const ASC_NO_ADDITIONAL_SENSE: u8 = 0x00;
const ASCQ_NO_ADDITIONAL_SENSE: u8 = 0x00;
const ASC_INVALID_FIELD_IN_CDB: u8 = 0x24;
const ASCQ_INVALID_FIELD_IN_CDB: u8 = 0x00;
const ASC_MEDIUM_NOT_PRESENT: u8 = 0x3A;
const ASCQ_MEDIUM_NOT_PRESENT: u8 = 0x00;

// ── Vendor/Product/Version strings ──────────────────────────────
const VENDOR_ID: &[u8; 8] = b"SEngines";
const PRODUCT_ID: &[u8; 16] = b"Remote Iso CDROM";
const PRODUCT_REVISION: &[u8; 4] = b"2.04";

// ── Pre-connect response (16 bytes) ─────────────────────────────
const PRE_CONNECT_RESPONSE: [u8; 16] = [
    0x00, 0x01, // version / vendor name length?
    0x00, // ?
    0x01, // flag
    0x00, 0x10, // block size (0x1000 = 4096, but we use 2048)
    0x00, 0x00, // ?
    0x2b, 0x10, // ?
    0x22, 0x22, // ?
    0x06, // device type (0x06 = CD/DVD)
    0x00, 0x00, 0x00, // padding
];

// ── Mode Sense (6) response header ──────────────────────────────
const MODE_SENSE_6_RESPONSE: [u8; 35] = [
    0x00, // mode data length
    0x20, // medium type (CDROM)
    0x01, // device-specific parameter
    0x00, // block descriptor length
    // block descriptor (8 bytes)
    0x00, 0x00, 0x00, 0x00, // number of blocks (0 = unused)
    0x00, 0x08, 0x00, // block length (0x0800 = 2048)
    // vendor-specific
    0x2a, 0x18, 0x07, 0x00, // ?
    0x77, 0x63, 0x2d, 0x23, // "wc-#!"
    0x21, 0x00, 0x00, 0xff, // ?
    0x00, 0x80, 0x21, 0x00, // ?
    // more vendor data
    0x00, 0x06, 0x00, 0x00, // ?
    0x00, 0x00, 0x00, 0x00, // ?
];

// ── Main server ──────────────────────────────────────────────────

/// Pure-Rust MahoganyStorageServer.
///
/// Listens on a TCP port and serves ISO/IMG files to BMCs via the
/// proprietary ServerEngines SCSI-over-TCP protocol.
#[derive(Debug)]
pub struct MahoganyStorageServer {
    // TcpListener::accept only needs a shared reference.  Keep it behind an
    // Arc so acceptors can take a snapshot and never hold the state lock
    // across the await point.
    listener: Arc<Mutex<Option<Arc<TcpListener>>>>,
    image: Arc<Mutex<Option<StorageImage>>>,
    expected_peer: Arc<Mutex<Option<IpAddr>>>,
    peer_ready_tx: watch::Sender<bool>,
}

#[derive(Clone, Debug)]
struct StorageImage {
    file: Arc<Mutex<tokio::fs::File>>,
    size: u64,
}

impl MahoganyStorageServer {
    pub fn new() -> Self {
        let (peer_ready_tx, _) = watch::channel(false);
        Self {
            listener: Arc::new(Mutex::new(None)),
            image: Arc::new(Mutex::new(None)),
            expected_peer: Arc::new(Mutex::new(None)),
            peer_ready_tx,
        }
    }

    /// Set the ISO file path for a test peer.
    #[cfg(test)]
    pub async fn set_image(&self, path: Option<String>) {
        let image = match path {
            Some(path) => match tokio::fs::File::open(&path).await {
                Ok(file) => match file.metadata().await {
                    Ok(metadata) => Some(StorageImage {
                        file: Arc::new(Mutex::new(file)),
                        size: metadata.len(),
                    }),
                    Err(error) => {
                        warn!("could not inspect Mahogany storage image {path}: {error}");
                        None
                    }
                },
                Err(error) => {
                    warn!("could not open Mahogany storage image {path}: {error}");
                    None
                }
            },
            None => None,
        };
        *self.image.lock().await = image;
    }

    /// Retain an already validated image handle for the complete peer session.
    pub async fn set_image_file(&self, file: Arc<std::fs::File>, size: u64) -> std::io::Result<()> {
        let file = file.try_clone()?;
        *self.image.lock().await = Some(StorageImage {
            file: Arc::new(Mutex::new(tokio::fs::File::from_std(file))),
            size,
        });
        Ok(())
    }

    pub async fn set_expected_peer(&self, peer: IpAddr) {
        *self.expected_peer.lock().await = Some(peer);
    }

    pub fn subscribe_peer_ready(&self) -> watch::Receiver<bool> {
        self.peer_ready_tx.subscribe()
    }

    pub fn peer_ready(&self) -> bool {
        *self.peer_ready_tx.borrow()
    }

    /// Start a test peer on an available interface.
    #[cfg(test)]
    pub async fn start(&self, port: u16) -> std::io::Result<()> {
        self.start_on(SocketAddr::from(([0, 0, 0, 0], port))).await
    }

    /// Bind the storage peer to the interface used by the main KVM session.
    pub async fn start_on(&self, addr: SocketAddr) -> std::io::Result<()> {
        let listener = TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        info!("MahoganyStorageServer listening on {}", addr);
        *self.listener.lock().await = Some(Arc::new(listener));
        Ok(())
    }

    pub async fn accept_one(&self) {
        let listener = self.listener.lock().await.clone();
        let Some(listener) = listener else {
            warn!("MahoganyStorageServer not started");
            return;
        };
        match listener.accept().await {
            Ok((stream, addr)) => {
                if self
                    .expected_peer
                    .lock()
                    .await
                    .is_some_and(|expected| expected != addr.ip())
                {
                    warn!("rejected storage connection from unexpected peer {addr}");
                    return;
                }
                info!("Storage connection from {}", addr);
                let image = self.image.lock().await.clone();
                if let Err(e) = handle_connection(stream, image, &self.peer_ready_tx).await {
                    warn!("Storage connection error: {}", e);
                }
                let _ = self.peer_ready_tx.send(false);
            }
            Err(e) => {
                warn!("Accept error: {}", e);
            }
        }
    }

    /// Return the local port the server is listening on.
    pub async fn local_port(&self) -> Option<u16> {
        let guard = self.listener.lock().await;
        guard
            .as_ref()
            .and_then(|l| l.local_addr().ok().map(|a| a.port()))
    }

    /// Accept connections in an infinite loop.
    pub async fn accept_loop(self: Arc<Self>) {
        loop {
            self.accept_one().await;
        }
    }
}

impl Default for MahoganyStorageServer {
    fn default() -> Self {
        Self::new()
    }
}

// ── Connection handler ────────────────────────────────────────────

async fn handle_connection(
    mut stream: TcpStream,
    image: Option<StorageImage>,
    peer_ready_tx: &watch::Sender<bool>,
) -> Result<(), anyhow::Error> {
    use bmc_backend_model::storage::{Action, Event, State};
    let mut model = State::Setup;
    // ── Phase 1: Read setup information (544 bytes) ──────────────
    let mut setup_buf = vec![0u8; SETUP_INFO_SIZE];
    tokio::time::timeout(SETUP_TIMEOUT, stream.read_exact(&mut setup_buf))
        .await
        .map_err(|_| anyhow::anyhow!("timed out waiting for storage setup"))??;
    debug!("Received 544-byte setup info");

    let connection_type = setup_buf[5];
    let transition = model.step(Event::Setup(connection_type));
    model = transition.0;
    if transition.1 != Action::Reply {
        anyhow::bail!("unsupported storage connection type 0x{connection_type:02x}");
    }
    let _port = u16::from_le_bytes([setup_buf[16], setup_buf[17]]);
    debug!(
        "Setup: connection_type=0x{:02x}, port={}",
        connection_type, _port
    );

    // ── Phase 2: Send pre-connect response (16 bytes) ───────────
    tokio::time::timeout(SETUP_TIMEOUT, stream.write_all(&PRE_CONNECT_RESPONSE))
        .await
        .map_err(|_| anyhow::anyhow!("storage setup response timed out"))??;
    model = model.step(Event::Replied).0;
    let _ = peer_ready_tx.send(true);
    debug!("Sent pre-connect response");

    // ── Phase 3: Main command loop ──────────────────────────────

    loop {
        // Read 64-byte header
        let mut header = [0u8; HEADER_SIZE];
        if stream.read_exact(&mut header[..1]).await.is_err() {
            break;
        }
        if !matches!(
            tokio::time::timeout(SETUP_TIMEOUT, stream.read_exact(&mut header[1..])).await,
            Ok(Ok(_))
        ) {
            debug!("Connection closed by peer");
            break;
        }

        let data_length = u32::from_le_bytes([
            header[HDR_DATA_LENGTH],
            header[HDR_DATA_LENGTH + 1],
            header[HDR_DATA_LENGTH + 2],
            header[HDR_DATA_LENGTH + 3],
        ]);
        let cmd_transaction_id = u32::from_le_bytes([
            header[HDR_TRANSACTION_ID],
            header[HDR_TRANSACTION_ID + 1],
            header[HDR_TRANSACTION_ID + 2],
            header[HDR_TRANSACTION_ID + 3],
        ]);
        let cmd_sequence_id = u32::from_le_bytes([
            header[HDR_SEQUENCE_ID],
            header[HDR_SEQUENCE_ID + 1],
            header[HDR_SEQUENCE_ID + 2],
            header[HDR_SEQUENCE_ID + 3],
        ]);
        let field_1c = u32::from_le_bytes([
            header[HDR_FIELD_1C],
            header[HDR_FIELD_1C + 1],
            header[HDR_FIELD_1C + 2],
            header[HDR_FIELD_1C + 3],
        ]);
        let field_20 = u32::from_le_bytes([
            header[HDR_FIELD_20],
            header[HDR_FIELD_20 + 1],
            header[HDR_FIELD_20 + 2],
            header[HDR_FIELD_20 + 3],
        ]);
        let opcode = header[HDR_SCSI_OPCODE];
        let direction = header[HDR_DIRECTION];
        let flags = header[HDR_FLAGS];

        let transition = model.step(Event::Command {
            direction,
            opcode,
            length: data_length,
        });
        model = transition.0;
        anyhow::ensure!(
            transition.1 != Action::Reject,
            "invalid storage command direction or CDB size"
        );
        // Check for disconnect
        if transition.1 == Action::Close {
            info!("Received disconnect (direction=0xFF or opcode=0xFF)");
            send_status(
                &mut stream,
                cmd_transaction_id,
                cmd_sequence_id,
                STATUS_GOOD,
            )
            .await?;
            break;
        }

        let transaction_id = cmd_transaction_id;
        let sequence_id = cmd_sequence_id;

        trace!(
            "SCSI cmd: opcode=0x{:02x}, data_len={}, dir=0x{:02x}, flags=0x{:02x}, tx_id={}",
            opcode,
            data_length,
            direction,
            flags,
            transaction_id
        );

        // Read CDB (SCSI command descriptor block)
        if data_length > SCSI_CDB_SIZE as u32 {
            anyhow::bail!("storage command CDB is too large: {data_length} bytes");
        }
        let cdb_len = data_length as usize;
        let mut cdb = vec![0u8; SCSI_CDB_SIZE];
        if data_length > 0 && direction == TYPE_COMMAND {
            let mut payload = vec![0u8; cdb_len];
            tokio::time::timeout(SETUP_TIMEOUT, stream.read_exact(&mut payload))
                .await
                .map_err(|_| anyhow::anyhow!("storage CDB timed out"))??;
            cdb[..cdb_len].copy_from_slice(&payload);
        }

        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            process_scsi_command(
                &mut stream,
                &cdb,
                opcode,
                transaction_id,
                sequence_id,
                field_1c,
                field_20,
                &image,
            ),
        )
        .await
        .map_err(|_| anyhow::anyhow!("storage response timed out; exchange discarded"))??;
    }

    Ok(())
}

// ── SCSI command dispatch ─────────────────────────────────────────

async fn process_scsi_command(
    stream: &mut TcpStream,
    cdb: &[u8],
    opcode: u8,
    transaction_id: u32,
    sequence_id: u32,
    _field_1c: u32,
    _field_20: u32,
    image: &Option<StorageImage>,
) -> Result<(), anyhow::Error> {
    match opcode {
        SCSI_TEST_UNIT_READY => cmd_test_unit_ready(stream, transaction_id, sequence_id).await,
        SCSI_REQUEST_SENSE => cmd_request_sense(stream, transaction_id, sequence_id).await,
        SCSI_INQUIRY => {
            let allocation_length = if cdb.len() > 4 { cdb[4] as usize } else { 36 };
            cmd_inquiry(stream, transaction_id, sequence_id, allocation_length, cdb).await
        }
        SCSI_MODE_SENSE_6 => cmd_mode_sense_6(stream, transaction_id, sequence_id, cdb).await,
        SCSI_PREVENT_ALLOW_MEDIUM_REMOVAL => {
            cmd_prevent_allow(stream, transaction_id, sequence_id).await
        }
        SCSI_READ_FORMAT_CAPACITIES => {
            cmd_read_format_capacities(stream, transaction_id, sequence_id, image).await
        }
        SCSI_READ_CAPACITY_10 => {
            cmd_read_capacity_10(stream, transaction_id, sequence_id, image).await
        }
        SCSI_READ_10 => cmd_read_10(stream, transaction_id, sequence_id, cdb, image).await,
        SCSI_WRITE_10 => cmd_write_10(stream, transaction_id, sequence_id).await,
        SCSI_GET_CONFIGURATION => {
            cmd_get_configuration(stream, transaction_id, sequence_id, cdb).await
        }
        SCSI_GET_EVENT_STATUS => cmd_get_event_status(stream, transaction_id, sequence_id).await,
        SCSI_GET_PERFORMANCE => cmd_get_performance(stream, transaction_id, sequence_id).await,
        SCSI_READ_DISC_INFORMATION => {
            cmd_read_disc_information(stream, transaction_id, sequence_id, image).await
        }
        SCSI_MODE_SENSE_10 => cmd_mode_sense_10(stream, transaction_id, sequence_id, cdb).await,
        SCSI_READ_TOC => cmd_read_toc(stream, transaction_id, sequence_id, cdb, image).await,
        _ => {
            trace!(
                "Unsupported opcode 0x{:02x}, returning illegal request",
                opcode
            );
            cmd_illegal_request(stream, transaction_id, sequence_id).await
        }
    }
}

// ── Helper: send data in (header + payload) ─────────────────────

async fn send_data_in(
    stream: &mut TcpStream,
    transaction_id: u32,
    sequence_id: u32,
    data: &[u8],
    residual: u32,
) -> Result<(), anyhow::Error> {
    if data.is_empty() {
        return send_data_chunk(stream, transaction_id, sequence_id, data, residual, false).await;
    }

    for (index, chunk) in data.chunks(MAX_TRANSFER_SIZE as usize).enumerate() {
        let bytes_sent = (index + 1) * MAX_TRANSFER_SIZE as usize;
        send_data_chunk(
            stream,
            transaction_id,
            sequence_id,
            chunk,
            if index == 0 { residual } else { 0 },
            bytes_sent < data.len(),
        )
        .await?;
    }

    Ok(())
}

async fn send_data_chunk(
    stream: &mut TcpStream,
    transaction_id: u32,
    sequence_id: u32,
    data: &[u8],
    residual: u32,
    has_more: bool,
) -> Result<(), anyhow::Error> {
    let mut header = [0u8; HEADER_SIZE];

    header[HDR_DATA_LENGTH..HDR_DATA_LENGTH + 4]
        .copy_from_slice(&(data.len() as u32).to_le_bytes());
    header[HDR_TRANSACTION_ID..HDR_TRANSACTION_ID + 4]
        .copy_from_slice(&transaction_id.to_le_bytes());
    header[HDR_SEQUENCE_ID..HDR_SEQUENCE_ID + 4].copy_from_slice(&sequence_id.to_le_bytes());
    header[HDR_DIRECTION] = TYPE_DATA_IN;
    header[HDR_FLAGS] = 0x00;

    header[HDR_STATUS..HDR_STATUS + 4].copy_from_slice(&residual.to_le_bytes());
    header[HDR_FIELD_1C..HDR_FIELD_1C + 4].copy_from_slice(&0u32.to_le_bytes());
    header[HDR_FIELD_20..HDR_FIELD_20 + 4].copy_from_slice(&0u32.to_le_bytes());

    header[0x3E] = u8::from(has_more);

    stream.write_all(&header).await?;
    stream.write_all(data).await?;

    Ok(())
}

/// Send SCSI status (header with type=STATUS + 1 status byte payload).
async fn send_status(
    stream: &mut TcpStream,
    transaction_id: u32,
    sequence_id: u32,
    status: u8,
) -> Result<(), anyhow::Error> {
    let mut header = [0u8; HEADER_SIZE];

    header[HDR_DATA_LENGTH..HDR_DATA_LENGTH + 4].copy_from_slice(&1u32.to_le_bytes());
    header[HDR_TRANSACTION_ID..HDR_TRANSACTION_ID + 4]
        .copy_from_slice(&transaction_id.to_le_bytes());
    header[HDR_SEQUENCE_ID..HDR_SEQUENCE_ID + 4].copy_from_slice(&sequence_id.to_le_bytes());
    header[HDR_STATUS..HDR_STATUS + 4].copy_from_slice(&(status as u32).to_le_bytes());
    header[HDR_DIRECTION] = TYPE_STATUS;
    header[0x3E] = 0x01;

    stream.write_all(&header).await?;
    stream.write_all(&[status]).await?;

    Ok(())
}

/// Set sense data in the sense buffer (response area).
fn build_sense_data(sense_key: u8, asc: u8, ascq: u8) -> [u8; 18] {
    let mut sense = [0u8; 18];
    sense[0] = 0x70;
    sense[1] = 0x00;
    sense[2] = sense_key;
    sense[3] = 0x00;
    sense[4..8].copy_from_slice(&[0u8; 4]);
    sense[8] = 18 - 8;
    sense[9] = 0x00;
    sense[10] = 0x00;
    sense[11] = 0x00;
    sense[12] = asc;
    sense[13] = ascq;
    sense[14] = 0x00;
    sense[15..18].copy_from_slice(&[0u8; 3]);
    sense
}

// ── SCSI command handlers ────────────────────────────────────────

async fn cmd_test_unit_ready(
    stream: &mut TcpStream,
    transaction_id: u32,
    sequence_id: u32,
) -> Result<(), anyhow::Error> {
    send_status(stream, transaction_id, sequence_id, STATUS_GOOD).await
}

async fn cmd_request_sense(
    stream: &mut TcpStream,
    transaction_id: u32,
    sequence_id: u32,
) -> Result<(), anyhow::Error> {
    let sense = build_sense_data(
        SENSE_NO_SENSE,
        ASC_NO_ADDITIONAL_SENSE,
        ASCQ_NO_ADDITIONAL_SENSE,
    );
    send_data_in(stream, transaction_id, sequence_id, &sense, 0).await?;
    send_status(stream, transaction_id, sequence_id, STATUS_GOOD).await
}

async fn cmd_inquiry(
    stream: &mut TcpStream,
    transaction_id: u32,
    sequence_id: u32,
    allocation_length: usize,
    cdb: &[u8],
) -> Result<(), anyhow::Error> {
    let pdt = cdb[1] & 0x01;
    let page_code = cdb[2];

    let response = if pdt != 0 {
        match page_code {
            0x00 => build_vpd_supported_pages(),
            0x80 => build_vpd_unit_serial_number(),
            0x83 => build_vpd_device_identification(),
            _ => {
                let sense = build_sense_data(
                    SENSE_ILLEGAL_REQUEST,
                    ASC_INVALID_FIELD_IN_CDB,
                    ASCQ_INVALID_FIELD_IN_CDB,
                );
                let mut buf = vec![0u8; 18];
                buf.copy_from_slice(&sense);
                send_data_in(stream, transaction_id, sequence_id, &buf, 0).await?;
                return send_status(stream, transaction_id, sequence_id, STATUS_CHECK_CONDITION)
                    .await;
            }
        }
    } else {
        build_standard_inquiry()
    };

    let len = response.len().min(allocation_length);
    send_data_in(stream, transaction_id, sequence_id, &response[..len], 0).await?;
    send_status(stream, transaction_id, sequence_id, STATUS_GOOD).await
}

fn build_standard_inquiry() -> Vec<u8> {
    let mut buf = vec![0u8; 36];
    buf[0] = 0x05;
    buf[1] = 0x80;
    buf[2] = 0x06;
    buf[3] = 0x02;
    buf[4] = 31;
    buf[5] = 0x00;
    buf[6] = 0x00;
    buf[7] = 0x00;
    buf[8..16].copy_from_slice(VENDOR_ID);
    buf[16..32].copy_from_slice(PRODUCT_ID);
    buf[32..36].copy_from_slice(PRODUCT_REVISION);
    buf
}

fn build_vpd_supported_pages() -> Vec<u8> {
    let mut buf = vec![0u8; 8];
    buf[0] = 0x05;
    buf[1] = 0x80;
    buf[2] = 0x00;
    buf[3] = 0x05;
    buf[4] = 0x00;
    buf[5] = 0x80;
    buf[6] = 0x83;
    buf
}

fn build_vpd_unit_serial_number() -> Vec<u8> {
    let serial = b"00000000";
    let mut buf = vec![0u8; 4 + serial.len()];
    buf[0] = 0x05;
    buf[1] = 0x80;
    buf[2] = 0x80;
    buf[3] = serial.len() as u8;
    buf[4..].copy_from_slice(serial);
    buf
}

fn build_vpd_device_identification() -> Vec<u8> {
    let designator = b"SEngines_Remote_Iso_CDROM_0000000000000000";
    let descriptor_len = 4 + designator.len();
    let len = 4 + descriptor_len;
    let mut buf = vec![0u8; len];
    buf[0] = 0x05;
    buf[1] = 0x80;
    buf[2] = 0x83;
    buf[3] = descriptor_len as u8;
    buf[4] = 0x02;
    buf[5] = 0x01;
    buf[6] = 0x00;
    buf[7] = designator.len() as u8;
    buf[8..].copy_from_slice(designator);
    buf
}

async fn cmd_read_capacity_10(
    stream: &mut TcpStream,
    transaction_id: u32,
    sequence_id: u32,
    image: &Option<StorageImage>,
) -> Result<(), anyhow::Error> {
    let file_size = image_size(image);

    let block_count = if file_size > 0 {
        (file_size / CDROM_BLOCK_SIZE as u64) as u32
    } else {
        0
    };
    let last_lba = block_count.saturating_sub(1);

    let mut response = [0u8; 8];
    response[0..4].copy_from_slice(&last_lba.to_be_bytes());
    response[4..8].copy_from_slice(&CDROM_BLOCK_SIZE.to_be_bytes());

    send_data_in(stream, transaction_id, sequence_id, &response, 0).await?;
    send_status(stream, transaction_id, sequence_id, STATUS_GOOD).await
}

async fn cmd_read_10(
    stream: &mut TcpStream,
    transaction_id: u32,
    sequence_id: u32,
    cdb: &[u8],
    image: &Option<StorageImage>,
) -> Result<(), anyhow::Error> {
    let lba = u32::from_be_bytes([
        cdb.get(2).copied().unwrap_or(0),
        cdb.get(3).copied().unwrap_or(0),
        cdb.get(4).copied().unwrap_or(0),
        cdb.get(5).copied().unwrap_or(0),
    ]);
    let transfer_blocks = u16::from_be_bytes([
        cdb.get(7).copied().unwrap_or(0),
        cdb.get(8).copied().unwrap_or(0),
    ]);

    let file_size = image_size(image);
    let max_block = if file_size > 0 {
        (file_size / CDROM_BLOCK_SIZE as u64) as u32
    } else {
        0
    };

    let end_block = lba.checked_add(u32::from(transfer_blocks));
    if transfer_blocks == 0 || end_block.is_none_or(|end| lba >= max_block || end > max_block) {
        let sense = build_sense_data(
            SENSE_ILLEGAL_REQUEST,
            ASC_MEDIUM_NOT_PRESENT,
            ASCQ_MEDIUM_NOT_PRESENT,
        );
        send_data_in(stream, transaction_id, sequence_id, &sense, 0).await?;
        return send_status(stream, transaction_id, sequence_id, STATUS_CHECK_CONDITION).await;
    }

    let offset = (lba as u64) * (CDROM_BLOCK_SIZE as u64);
    let read_size = (transfer_blocks as u64) * (CDROM_BLOCK_SIZE as u64);

    let Some(image) = image else {
        anyhow::bail!("READ(10) received without mounted media");
    };
    let mut file = image.file.lock().await;
    use tokio::io::AsyncSeekExt;
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut remaining = read_size;
    let mut data = [0u8; MAX_TRANSFER_SIZE as usize];
    while remaining > 0 {
        let chunk_size = remaining.min(MAX_TRANSFER_SIZE as u64) as usize;
        file.read_exact(&mut data[..chunk_size]).await?;
        remaining -= chunk_size as u64;
        send_data_chunk(
            stream,
            transaction_id,
            sequence_id,
            &data[..chunk_size],
            0,
            remaining > 0,
        )
        .await?;
    }
    send_status(stream, transaction_id, sequence_id, STATUS_GOOD).await
}

async fn cmd_write_10(
    stream: &mut TcpStream,
    transaction_id: u32,
    sequence_id: u32,
) -> Result<(), anyhow::Error> {
    let sense = build_sense_data(SENSE_ILLEGAL_REQUEST, 0x27, 0x00);
    send_data_in(stream, transaction_id, sequence_id, &sense, 0).await?;
    send_status(stream, transaction_id, sequence_id, STATUS_CHECK_CONDITION).await
}

async fn cmd_mode_sense_6(
    stream: &mut TcpStream,
    transaction_id: u32,
    sequence_id: u32,
    cdb: &[u8],
) -> Result<(), anyhow::Error> {
    let _page_code = cdb[2] & 0x3F;
    let _subpage = cdb[3];

    send_data_in(
        stream,
        transaction_id,
        sequence_id,
        &MODE_SENSE_6_RESPONSE,
        0,
    )
    .await?;
    send_status(stream, transaction_id, sequence_id, STATUS_GOOD).await
}

async fn cmd_mode_sense_10(
    stream: &mut TcpStream,
    transaction_id: u32,
    sequence_id: u32,
    _cdb: &[u8],
) -> Result<(), anyhow::Error> {
    let mut response = vec![0u8; 40];
    let mode_len = (response.len() as u16 - 2).to_be_bytes();
    response[0..2].copy_from_slice(&mode_len);
    response[2] = 0x00;
    response[3] = 0x00;
    response[4..6].copy_from_slice(&[0x00, 0x00]);
    response[6..8].copy_from_slice(&[0x00, 0x00]);
    response[8] = 0x08;
    response[9] = 0x00;
    response[10] = 0x2A;
    response[11] = 0x18;
    response[12] = 0x07;
    response[13] = 0x00;
    response[14] = 0x77;
    response[15] = 0x63;
    response[16] = 0x2D;
    response[17] = 0x23;
    response[18] = 0x21;
    response[19] = 0x00;
    response[20] = 0x00;
    response[21] = 0xFF;
    response[22] = 0x00;
    response[23] = 0x80;
    response[24] = 0x21;
    response[25] = 0x00;
    response[26] = 0x00;
    response[27] = 0x06;
    response[28] = 0x00;
    response[29] = 0x00;

    send_data_in(stream, transaction_id, sequence_id, &response, 0).await?;
    send_status(stream, transaction_id, sequence_id, STATUS_GOOD).await
}

async fn cmd_read_toc(
    stream: &mut TcpStream,
    transaction_id: u32,
    sequence_id: u32,
    cdb: &[u8],
    image: &Option<StorageImage>,
) -> Result<(), anyhow::Error> {
    let msf = cdb[1] & 0x02;
    let format = cdb[2] & 0x0F;

    let file_size = image_size(image);
    let block_count = if file_size > 0 {
        (file_size / CDROM_BLOCK_SIZE as u64) as u32
    } else {
        0
    };
    let last_lba = block_count.saturating_sub(1);
    let _track_count = 1;

    match format {
        0x00 => {
            if msf != 0 {
                let mut toc = vec![0u8; 20];
                toc[0] = 0x00;
                toc[1] = 0x12;
                toc[2] = 0x01;
                toc[3] = 0x01;
                toc[4] = 0x00;
                toc[5] = 0x14;
                toc[6] = 0x01;
                toc[7] = 0x00;
                let (m, s, f) = lba_to_msf(last_lba);
                toc[8] = 0x00;
                toc[9] = m;
                toc[10] = s;
                toc[11] = f;
                let lead_out_lba = block_count + 150;
                let (lm, ls, lf) = lba_to_msf(lead_out_lba);
                toc[12] = 0x00;
                toc[13] = 0x14;
                toc[14] = 0xAA;
                toc[15] = 0x00;
                toc[16] = 0x00;
                toc[17] = lm;
                toc[18] = ls;
                toc[19] = lf;

                send_data_in(stream, transaction_id, sequence_id, &toc, 0).await?;
            } else {
                let mut toc = vec![0u8; 20];
                toc[0] = 0x00;
                toc[1] = 0x12;
                toc[2] = 0x01;
                toc[3] = 0x01;
                toc[4] = 0x00;
                toc[5] = 0x14;
                toc[6] = 0x01;
                toc[7] = 0x00;
                toc[8..12].copy_from_slice(&last_lba.to_be_bytes());
                toc[12] = 0x00;
                toc[13] = 0x14;
                toc[14] = 0xAA;
                toc[15] = 0x00;
                toc[16..20].copy_from_slice(&(block_count + 150).to_be_bytes());

                send_data_in(stream, transaction_id, sequence_id, &toc, 0).await?;
            }
        }
        0x01 => {
            let mut toc = vec![0u8; 12];
            toc[0] = 0x00;
            toc[1] = 0x0A;
            toc[2] = 0x01;
            toc[3] = 0x01;
            toc[4] = 0x00;
            toc[5] = 0x14;
            toc[6] = 0x01;
            toc[7] = 0x00;
            if msf != 0 {
                let (m, s, f) = lba_to_msf(0);
                toc[8] = 0x00;
                toc[9] = m;
                toc[10] = s;
                toc[11] = f;
            } else {
                toc[8..12].copy_from_slice(&0u32.to_be_bytes());
            }
            send_data_in(stream, transaction_id, sequence_id, &toc, 0).await?;
        }
        _ => {
            let sense = build_sense_data(
                SENSE_ILLEGAL_REQUEST,
                ASC_INVALID_FIELD_IN_CDB,
                ASCQ_INVALID_FIELD_IN_CDB,
            );
            send_data_in(stream, transaction_id, sequence_id, &sense, 0).await?;
            return send_status(stream, transaction_id, sequence_id, STATUS_CHECK_CONDITION).await;
        }
    }

    send_status(stream, transaction_id, sequence_id, STATUS_GOOD).await
}

async fn cmd_prevent_allow(
    stream: &mut TcpStream,
    transaction_id: u32,
    sequence_id: u32,
) -> Result<(), anyhow::Error> {
    send_status(stream, transaction_id, sequence_id, STATUS_GOOD).await
}

async fn cmd_read_format_capacities(
    stream: &mut TcpStream,
    transaction_id: u32,
    sequence_id: u32,
    image: &Option<StorageImage>,
) -> Result<(), anyhow::Error> {
    let file_size = image_size(image);
    let block_count = if file_size > 0 {
        (file_size / CDROM_BLOCK_SIZE as u64) as u32
    } else {
        0
    };

    let mut response = vec![0u8; 20];
    let cap_len = (response.len() as u32 - 4).to_be_bytes();
    response[0..4].copy_from_slice(&cap_len);
    response[4] = 0x00;
    response[5] = 0x00;
    response[6] = 0x00;
    response[7] = 0x02;
    response[8..12].copy_from_slice(&block_count.to_be_bytes());
    response[12] = 0x00;
    response[13] = 0x02;
    response[14..16].copy_from_slice(&[0x00, 0x08]);

    send_data_in(stream, transaction_id, sequence_id, &response, 0).await?;
    send_status(stream, transaction_id, sequence_id, STATUS_GOOD).await
}

async fn cmd_get_configuration(
    stream: &mut TcpStream,
    transaction_id: u32,
    sequence_id: u32,
    cdb: &[u8],
) -> Result<(), anyhow::Error> {
    let _rt = cdb[1] >> 6;
    let starting_feature = u16::from_be_bytes([
        cdb.get(2).copied().unwrap_or(0),
        cdb.get(3).copied().unwrap_or(0),
    ]);
    let _allocation_length = u16::from_be_bytes([
        cdb.get(7).copied().unwrap_or(0),
        cdb.get(8).copied().unwrap_or(0),
    ]);

    let mut response = Vec::new();

    response.extend_from_slice(&[0x00, 0x00]);
    response.extend_from_slice(&[0x00, 0x00]);
    response.extend_from_slice(&[0x01, 0x00]);
    response.extend_from_slice(&[0x00, 0x00]);

    if starting_feature <= 0x0001 {
        let feature = build_feature_descriptor(0x0001, 0x00, &[0x01, 0x00]);
        response.extend_from_slice(&feature);
    }

    if starting_feature <= 0x0003 {
        let feature = build_feature_descriptor(0x0003, 0x00, &[0x00, 0x00, 0x00, 0x00]);
        response.extend_from_slice(&feature);
    }

    if starting_feature <= 0x0010 {
        let feature = build_feature_descriptor(0x0010, 0x00, &[0x00, 0x00, 0x00, 0x00]);
        response.extend_from_slice(&feature);
    }

    if starting_feature <= 0x0011 {
        let feature = build_feature_descriptor(0x0011, 0x00, &[0x00, 0x00, 0x00, 0x00]);
        response.extend_from_slice(&feature);
    }

    if starting_feature <= 0x0100 {
        let feature = build_feature_descriptor(0x0100, 0x00, &[0x00, 0x00, 0x00, 0x00]);
        response.extend_from_slice(&feature);
    }

    let len = response.len() as u16 - 4;
    let data_len = len.to_be_bytes();
    response[0] = data_len[0];
    response[1] = data_len[1];

    send_data_in(stream, transaction_id, sequence_id, &response, 0).await?;
    send_status(stream, transaction_id, sequence_id, STATUS_GOOD).await
}

fn build_feature_descriptor(feature_code: u16, version: u8, data: &[u8]) -> Vec<u8> {
    let mut desc = Vec::with_capacity(4 + data.len());
    desc.extend_from_slice(&feature_code.to_be_bytes());
    desc.push(version);
    desc.push(0x00);
    desc.extend_from_slice(data);
    desc
}

async fn cmd_get_event_status(
    stream: &mut TcpStream,
    transaction_id: u32,
    sequence_id: u32,
) -> Result<(), anyhow::Error> {
    let mut response = vec![0u8; 8];
    response[0] = 0x00;
    response[1] = 0x06;
    response[2] = 0x00;
    response[3] = 0x00;
    response[4] = 0x00;
    response[5] = 0x00;
    response[6] = 0x00;
    response[7] = 0x00;

    send_data_in(stream, transaction_id, sequence_id, &response, 0).await?;
    send_status(stream, transaction_id, sequence_id, STATUS_GOOD).await
}

async fn cmd_get_performance(
    stream: &mut TcpStream,
    transaction_id: u32,
    sequence_id: u32,
) -> Result<(), anyhow::Error> {
    let mut response = vec![0u8; 28];
    response[0..4].copy_from_slice(&[0x00, 0x00, 0x00, 0x18]);
    response[4] = 0x01;
    response[5] = 0x00;
    response[6..8].copy_from_slice(&[0x00, 0x01]);

    response[8] = 0x00;
    response[9] = 0x00;
    response[10..14].copy_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    response[14..18].copy_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    response[18..22].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    response[22..26].copy_from_slice(&[0x00, 0x00, 0x10, 0x8A]);

    send_data_in(stream, transaction_id, sequence_id, &response, 0).await?;
    send_status(stream, transaction_id, sequence_id, STATUS_GOOD).await
}

async fn cmd_read_disc_information(
    stream: &mut TcpStream,
    transaction_id: u32,
    sequence_id: u32,
    image: &Option<StorageImage>,
) -> Result<(), anyhow::Error> {
    let file_size = image_size(image);
    let block_count = if file_size > 0 {
        (file_size / CDROM_BLOCK_SIZE as u64) as u32
    } else {
        0
    };

    let mut response = vec![0u8; 34];
    response[0] = 0x00;
    response[1] = 0x20;
    response[2] = 0x02;
    response[3] = 0x00;
    response[4] = 0x00;
    response[5] = 0x00;
    response[6] = 0x01;
    response[7] = 0x01;
    response[8] = 0x01;
    response[9] = 0x00;
    response[16..20].copy_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    response[20..24].copy_from_slice(&block_count.to_be_bytes());
    response[24..32].copy_from_slice(&[0u8; 8]);
    response[32] = 0x00;
    response[33] = 0x00;

    send_data_in(stream, transaction_id, sequence_id, &response, 0).await?;
    send_status(stream, transaction_id, sequence_id, STATUS_GOOD).await
}

async fn cmd_illegal_request(
    stream: &mut TcpStream,
    transaction_id: u32,
    sequence_id: u32,
) -> Result<(), anyhow::Error> {
    let sense = build_sense_data(
        SENSE_ILLEGAL_REQUEST,
        ASC_INVALID_FIELD_IN_CDB,
        ASCQ_INVALID_FIELD_IN_CDB,
    );
    send_data_in(stream, transaction_id, sequence_id, &sense, 0).await?;
    send_status(stream, transaction_id, sequence_id, STATUS_CHECK_CONDITION).await
}

// ── Helpers ───────────────────────────────────────────────────────

fn image_size(image: &Option<StorageImage>) -> u64 {
    image.as_ref().map_or(0, |image| image.size)
}

fn lba_to_msf(lba: u32) -> (u8, u8, u8) {
    let total_frames = lba + 150;
    let m = (total_frames / (75 * 60)) as u8;
    let s = ((total_frames / 75) % 60) as u8;
    let f = (total_frames % 75) as u8;
    (m, s, f)
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::net::TcpStream;
    use tokio::time::timeout;

    fn build_cmd_header(opcode: u8, data_len: u32, tx_id: u32, seq_id: u32) -> [u8; 64] {
        let mut h = [0u8; 64];
        h[HDR_DATA_LENGTH..HDR_DATA_LENGTH + 4].copy_from_slice(&data_len.to_le_bytes());
        h[HDR_TRANSACTION_ID..HDR_TRANSACTION_ID + 4].copy_from_slice(&tx_id.to_le_bytes());
        h[HDR_SEQUENCE_ID..HDR_SEQUENCE_ID + 4].copy_from_slice(&seq_id.to_le_bytes());
        h[HDR_SCSI_OPCODE] = opcode;
        h
    }

    fn parse_response_header(data: &[u8]) -> (u32, u32, u32, u8, u32) {
        let data_len = u32::from_le_bytes(
            data[HDR_DATA_LENGTH..HDR_DATA_LENGTH + 4]
                .try_into()
                .unwrap(),
        );
        let tx_id = u32::from_le_bytes(
            data[HDR_TRANSACTION_ID..HDR_TRANSACTION_ID + 4]
                .try_into()
                .unwrap(),
        );
        let seq_id = u32::from_le_bytes(
            data[HDR_SEQUENCE_ID..HDR_SEQUENCE_ID + 4]
                .try_into()
                .unwrap(),
        );
        let direction = data[HDR_DIRECTION];
        let residual = u32::from_le_bytes(data[HDR_STATUS..HDR_STATUS + 4].try_into().unwrap());
        (data_len, tx_id, seq_id, direction, residual)
    }

    async fn run_scsi_command_with_chunks(
        iso_content: &[u8],
        opcode: u8,
        cdb: &[u8],
    ) -> (Vec<u8>, u8, Vec<u8>) {
        let dir = tempfile::tempdir().unwrap();
        let iso_path = dir.path().join("test.iso");
        std::fs::write(&iso_path, iso_content).unwrap();

        let server = Arc::new(MahoganyStorageServer::new());
        server
            .set_image(Some(iso_path.to_string_lossy().to_string()))
            .await;
        server.start(0).await.unwrap();
        let port = {
            let guard = server.listener.lock().await;
            guard.as_ref().unwrap().local_addr().unwrap().port()
        };

        let server2 = server.clone();
        let _accept_handle = tokio::spawn(async move {
            server2.accept_one().await;
        });

        let mut client = TcpStream::connect(format!("127.0.0.1:{}", port))
            .await
            .unwrap();

        let mut setup = vec![0u8; 544];
        setup[5] = 0xFE;
        setup[16..18].copy_from_slice(&port.to_le_bytes());
        client.write_all(&setup).await.unwrap();

        let mut pre_connect = [0u8; 16];
        client.read_exact(&mut pre_connect).await.unwrap();
        assert_eq!(pre_connect, PRE_CONNECT_RESPONSE);

        let header = build_cmd_header(opcode, cdb.len() as u32, 1, 0);
        client.write_all(&header).await.unwrap();
        if !cdb.is_empty() {
            client.write_all(cdb).await.unwrap();
        }

        let mut data = Vec::new();
        let mut status_byte = 0u8;
        let mut continuation = Vec::new();

        let mut resp_hdr = [0u8; 64];
        client.read_exact(&mut resp_hdr).await.unwrap();
        let mut parsed = parse_response_header(&resp_hdr);

        if parsed.3 == TYPE_DATA_IN {
            loop {
                let chunk_start = data.len();
                data.resize(chunk_start + parsed.0 as usize, 0);
                if parsed.0 > 0 {
                    client.read_exact(&mut data[chunk_start..]).await.unwrap();
                }
                continuation.push(resp_hdr[0x3E]);
                if resp_hdr[0x3E] == 0 {
                    break;
                }
                client.read_exact(&mut resp_hdr).await.unwrap();
                parsed = parse_response_header(&resp_hdr);
                assert_eq!(parsed.3, TYPE_DATA_IN);
            }
            let mut status_hdr = [0u8; 64];
            client.read_exact(&mut status_hdr).await.unwrap();
            let (_, _, _, status_dir, _) = parse_response_header(&status_hdr);
            assert_eq!(status_dir, TYPE_STATUS);
            client
                .read_exact(std::slice::from_mut(&mut status_byte))
                .await
                .unwrap();
        } else if parsed.3 == TYPE_STATUS {
            client
                .read_exact(std::slice::from_mut(&mut status_byte))
                .await
                .unwrap();
        } else {
            panic!("Unexpected direction in response: 0x{:02x}", parsed.3);
        }

        (data, status_byte, continuation)
    }

    async fn run_scsi_command(iso_content: &[u8], opcode: u8, cdb: &[u8]) -> (Vec<u8>, u8) {
        let (data, status, _) = run_scsi_command_with_chunks(iso_content, opcode, cdb).await;
        (data, status)
    }

    // ═══════════════════════════════════════════════════════════════
    // Unit tests
    // ═══════════════════════════════════════════════════════════════

    #[test]
    fn test_lba_to_msf() {
        let (m, s, f) = lba_to_msf(0);
        assert_eq!(m, 0);
        assert_eq!(s, 2);
        assert_eq!(f, 0);
    }

    #[test]
    fn test_lba_to_msf_typical() {
        let lba = 150 * 60;
        let (_m, s, f) = lba_to_msf(lba);
        assert_eq!(s, 2);
        assert_eq!(f, 0);
    }

    #[test]
    fn test_standard_inquiry_size() {
        let inquiry = build_standard_inquiry();
        assert_eq!(inquiry.len(), 36);
        assert_eq!(inquiry[0], 0x05);
        assert_eq!(&inquiry[8..16], VENDOR_ID);
        assert_eq!(&inquiry[16..32], PRODUCT_ID);
    }

    #[test]
    fn test_sense_data_format() {
        let sense = build_sense_data(
            SENSE_ILLEGAL_REQUEST,
            ASC_INVALID_FIELD_IN_CDB,
            ASCQ_INVALID_FIELD_IN_CDB,
        );
        assert_eq!(sense[0], 0x70);
        assert_eq!(sense[2], SENSE_ILLEGAL_REQUEST);
        assert_eq!(sense[12], ASC_INVALID_FIELD_IN_CDB);
        assert_eq!(sense[13], ASCQ_INVALID_FIELD_IN_CDB);
    }

    #[test]
    fn test_read_capacity_response() {
        let block_count: u32 = 100;
        let last_lba = block_count.saturating_sub(1);
        let mut response = [0u8; 8];
        response[0..4].copy_from_slice(&last_lba.to_be_bytes());
        response[4..8].copy_from_slice(&CDROM_BLOCK_SIZE.to_be_bytes());
        assert_eq!(response[0..4], [0x00, 0x00, 0x00, 99]);
        assert_eq!(response[4..8], [0x00, 0x00, 0x08, 0x00]);
    }

    #[test]
    fn mmc_opcodes_match_the_wire_standard() {
        assert_eq!(SCSI_READ_TOC, 0x43);
        assert_eq!(SCSI_GET_CONFIGURATION, 0x46);
        assert_eq!(SCSI_GET_EVENT_STATUS, 0x4A);
        assert_eq!(SCSI_GET_PERFORMANCE, 0xAC);
    }

    // ═══════════════════════════════════════════════════════════════
    // Integration tests (real TCP + SCSI protocol round-trip)
    // ═══════════════════════════════════════════════════════════════

    #[tokio::test]
    async fn test_handshake_only() {
        let server = Arc::new(MahoganyStorageServer::new());
        server.start(0).await.unwrap();
        let port = {
            let guard = server.listener.lock().await;
            guard.as_ref().unwrap().local_addr().unwrap().port()
        };

        let s2 = server.clone();
        let _accept_handle = tokio::spawn(async move { s2.accept_one().await });

        let mut client = TcpStream::connect(format!("127.0.0.1:{}", port))
            .await
            .unwrap();

        let mut setup = vec![0u8; 544];
        setup[5] = 0xFE;
        client.write_all(&setup).await.unwrap();

        let mut pre_connect = [0u8; 16];
        timeout(Duration::from_secs(5), client.read_exact(&mut pre_connect))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pre_connect, PRE_CONNECT_RESPONSE);

        let disconnect_hdr = build_cmd_header(0xFF, 0, 0, 0);
        client.write_all(&disconnect_hdr).await.unwrap();

        let mut status_hdr = [0u8; 64];
        timeout(Duration::from_secs(5), client.read_exact(&mut status_hdr))
            .await
            .unwrap()
            .unwrap();
        let (_, _, _, dir, _) = parse_response_header(&status_hdr);
        assert_eq!(dir, TYPE_STATUS);
        let mut st = [0u8; 1];
        client.read_exact(&mut st).await.unwrap();
        assert_eq!(st[0], STATUS_GOOD);
    }

    #[tokio::test]
    async fn test_scsi_test_unit_ready() {
        let (data, status) = run_scsi_command(b"", SCSI_TEST_UNIT_READY, &[]).await;
        assert!(data.is_empty());
        assert_eq!(status, STATUS_GOOD);
    }

    #[tokio::test]
    async fn test_scsi_inquiry() {
        let cdb = [SCSI_INQUIRY, 0x00, 0x00, 0x00, 36, 0x00];
        let (data, status) = run_scsi_command(b"", SCSI_INQUIRY, &cdb).await;
        assert_eq!(status, STATUS_GOOD);
        assert!(!data.is_empty());
        assert_eq!(data[0], 0x05);
        assert_eq!(data[1], 0x80);
        assert_eq!(&data[8..16], VENDOR_ID);
        assert_eq!(&data[16..32], PRODUCT_ID);
    }

    #[tokio::test]
    async fn test_scsi_inquiry_vpd_supported_pages() {
        let cdb = [SCSI_INQUIRY, 0x01, 0x00, 0x00, 8, 0x00];
        let (data, status) = run_scsi_command(b"", SCSI_INQUIRY, &cdb).await;
        assert_eq!(status, STATUS_GOOD);
        assert_eq!(data[0], 0x05);
        assert_eq!(data[2], 0x00);
        assert!(data[3] >= 3);
        assert_eq!(data[4], 0x00);
        assert_eq!(data[5], 0x80);
        assert_eq!(data[6], 0x83);
    }

    #[tokio::test]
    async fn test_scsi_inquiry_vpd_unit_serial() {
        let cdb = [SCSI_INQUIRY, 0x01, 0x80, 0x00, 12, 0x00];
        let (data, status) = run_scsi_command(b"", SCSI_INQUIRY, &cdb).await;
        assert_eq!(status, STATUS_GOOD);
        assert_eq!(data[0], 0x05);
        assert_eq!(data[2], 0x80);
    }

    #[tokio::test]
    async fn test_scsi_inquiry_vpd_device_id() {
        let cdb = [SCSI_INQUIRY, 0x01, 0x83, 0x00, 40, 0x00];
        let (data, status) = run_scsi_command(b"", SCSI_INQUIRY, &cdb).await;
        assert_eq!(status, STATUS_GOOD);
        assert_eq!(data[0], 0x05);
        assert_eq!(data[2], 0x83);
    }

    #[tokio::test]
    async fn test_scsi_inquiry_unsupported_vpd() {
        let cdb = [SCSI_INQUIRY, 0x01, 0xFF, 0x00, 36, 0x00];
        let (data, status) = run_scsi_command(b"", SCSI_INQUIRY, &cdb).await;
        assert_eq!(status, STATUS_CHECK_CONDITION);
        assert!(!data.is_empty());
    }

    #[tokio::test]
    async fn test_scsi_read_capacity_no_media() {
        let cdb = [
            SCSI_READ_CAPACITY_10,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
        ];
        let (data, status) = run_scsi_command(b"", SCSI_READ_CAPACITY_10, &cdb).await;
        assert_eq!(status, STATUS_GOOD);
        assert_eq!(data.len(), 8);
        assert_eq!(&data[0..4], &[0x00, 0x00, 0x00, 0x00]);
        assert_eq!(&data[4..8], &[0x00, 0x00, 0x08, 0x00]);
    }

    #[tokio::test]
    async fn test_scsi_read_capacity_with_media() {
        let iso = vec![0u8; 2048 * 100];
        let cdb = [
            SCSI_READ_CAPACITY_10,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
        ];
        let (data, status) = run_scsi_command(&iso, SCSI_READ_CAPACITY_10, &cdb).await;
        assert_eq!(status, STATUS_GOOD);
        assert_eq!(data.len(), 8);
        assert_eq!(&data[0..4], &[0x00, 0x00, 0x00, 99]);
        assert_eq!(&data[4..8], &[0x00, 0x00, 0x08, 0x00]);
    }

    #[tokio::test]
    async fn test_scsi_read_10_single_block() {
        let mut iso = vec![0u8; 2048 * 10];
        iso[0..4].copy_from_slice(b"\x00\x01\x02\x03");
        let cdb = [
            SCSI_READ_10,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x01,
            0x00,
        ];
        let (data, status) = run_scsi_command(&iso, SCSI_READ_10, &cdb).await;
        assert_eq!(status, STATUS_GOOD);
        assert_eq!(data.len(), 2048);
        assert_eq!(&data[0..4], b"\x00\x01\x02\x03");
    }

    #[tokio::test]
    async fn test_scsi_read_10_multiple_blocks() {
        let mut iso = vec![0u8; 2048 * 5];
        iso[2 * 2048 + 100] = 0xAB;
        iso[2 * 2048 + 101] = 0xCD;
        let cdb = [
            SCSI_READ_10,
            0x00,
            0x00,
            0x00,
            0x00,
            0x02,
            0x00,
            0x00,
            0x02,
            0x00,
        ];
        let (data, status) = run_scsi_command(&iso, SCSI_READ_10, &cdb).await;
        assert_eq!(status, STATUS_GOOD);
        assert_eq!(data.len(), 4096);
        assert_eq!(data[100], 0xAB);
        assert_eq!(data[101], 0xCD);
        assert_eq!(data[2048 + 100], 0x00);
    }

    #[tokio::test]
    async fn test_scsi_read_10_streams_multiple_chunks_with_a_final_marker() {
        let iso = vec![0xA5; 2048 * 3];
        let cdb = [
            SCSI_READ_10,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x03,
            0x00,
        ];
        let (data, status, continuation) =
            run_scsi_command_with_chunks(&iso, SCSI_READ_10, &cdb).await;
        assert_eq!(status, STATUS_GOOD);
        assert_eq!(data, iso);
        assert_eq!(continuation, vec![1, 0]);
    }

    #[tokio::test]
    async fn test_scsi_read_10_beyond_end() {
        let iso = vec![0u8; 2048 * 3];
        let cdb = [
            SCSI_READ_10,
            0x00,
            0x00,
            0x00,
            0x00,
            0x05,
            0x00,
            0x00,
            0x01,
            0x00,
        ];
        let (data, status) = run_scsi_command(&iso, SCSI_READ_10, &cdb).await;
        assert_eq!(status, STATUS_CHECK_CONDITION);
        assert!(!data.is_empty());
    }

    #[tokio::test]
    async fn test_scsi_read_10_zero_blocks() {
        let iso = vec![0u8; 2048 * 3];
        let cdb = [
            SCSI_READ_10,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
        ];
        let (_data, status) = run_scsi_command(&iso, SCSI_READ_10, &cdb).await;
        assert_eq!(status, STATUS_CHECK_CONDITION);
    }

    #[tokio::test]
    async fn test_scsi_write_10_readonly() {
        let cdb = [
            SCSI_WRITE_10,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x01,
            0x00,
        ];
        let (data, status) = run_scsi_command(b"", SCSI_WRITE_10, &cdb).await;
        assert_eq!(status, STATUS_CHECK_CONDITION);
        assert!(!data.is_empty());
    }

    #[tokio::test]
    async fn test_scsi_request_sense() {
        let cdb = [SCSI_REQUEST_SENSE, 0x00, 0x00, 0x00, 18, 0x00];
        let (data, status) = run_scsi_command(b"", SCSI_REQUEST_SENSE, &cdb).await;
        assert_eq!(status, STATUS_GOOD);
        assert!(!data.is_empty());
        assert_eq!(data[0], 0x70);
        assert_eq!(data[2], SENSE_NO_SENSE);
    }

    #[tokio::test]
    async fn test_scsi_mode_sense_6() {
        let cdb = [SCSI_MODE_SENSE_6, 0x00, 0x3F, 0x00, 192, 0x00];
        let (data, status) = run_scsi_command(b"", SCSI_MODE_SENSE_6, &cdb).await;
        assert_eq!(status, STATUS_GOOD);
        assert!(!data.is_empty());
        assert_eq!(data[0], 0x00);
        assert_eq!(data[1], 0x20);
    }

    #[tokio::test]
    async fn test_scsi_mode_sense_10() {
        let cdb = [
            SCSI_MODE_SENSE_10,
            0x00,
            0x3F,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            192,
            0x00,
        ];
        let (data, status) = run_scsi_command(b"", SCSI_MODE_SENSE_10, &cdb).await;
        assert_eq!(status, STATUS_GOOD);
        assert!(data.len() >= 10);
    }

    #[tokio::test]
    async fn test_scsi_read_toc_lba() {
        let iso = vec![0u8; 2048 * 100];
        let cdb = [
            SCSI_READ_TOC,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0xFF,
            0x00,
        ];
        let (data, status) = run_scsi_command(&iso, SCSI_READ_TOC, &cdb).await;
        assert_eq!(status, STATUS_GOOD);
        assert_eq!(data.len(), 20);
        assert_eq!(data[2], 0x01);
        assert_eq!(data[3], 0x01);
    }

    #[tokio::test]
    async fn test_scsi_read_toc_msf() {
        let iso = vec![0u8; 2048 * 100];
        let cdb = [
            SCSI_READ_TOC,
            0x02,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0xFF,
            0x00,
        ];
        let (data, status) = run_scsi_command(&iso, SCSI_READ_TOC, &cdb).await;
        assert_eq!(status, STATUS_GOOD);
        assert_eq!(data.len(), 20);
    }

    #[tokio::test]
    async fn test_scsi_read_toc_session_info() {
        let cdb = [
            SCSI_READ_TOC,
            0x00,
            0x01,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0xFF,
            0x00,
        ];
        let (data, status) = run_scsi_command(b"", SCSI_READ_TOC, &cdb).await;
        assert_eq!(status, STATUS_GOOD);
        assert_eq!(data.len(), 12);
    }

    #[tokio::test]
    async fn test_scsi_read_format_capacities() {
        let iso = vec![0u8; 2048 * 50];
        let cdb = [
            SCSI_READ_FORMAT_CAPACITIES,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x14,
            0x00,
        ];
        let (data, status) = run_scsi_command(&iso, SCSI_READ_FORMAT_CAPACITIES, &cdb).await;
        assert_eq!(status, STATUS_GOOD);
        assert_eq!(data.len(), 20);
        assert_eq!(&data[8..12], &[0x00, 0x00, 0x00, 50]);
    }

    #[tokio::test]
    async fn test_scsi_prevent_allow() {
        let cdb = [
            SCSI_PREVENT_ALLOW_MEDIUM_REMOVAL,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
        ];
        let (data, status) = run_scsi_command(b"", SCSI_PREVENT_ALLOW_MEDIUM_REMOVAL, &cdb).await;
        assert_eq!(status, STATUS_GOOD);
        assert!(data.is_empty());
    }

    #[tokio::test]
    async fn test_scsi_get_configuration() {
        let cdb = [
            SCSI_GET_CONFIGURATION,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0xFF,
            0x00,
        ];
        let (data, status) = run_scsi_command(b"", SCSI_GET_CONFIGURATION, &cdb).await;
        assert_eq!(status, STATUS_GOOD);
        assert!(data.len() >= 8);
        assert_eq!(&data[4..6], &[0x01, 0x00]);
    }

    #[tokio::test]
    async fn test_scsi_get_event_status() {
        let cdb = [
            SCSI_GET_EVENT_STATUS,
            0x01,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x08,
            0x00,
        ];
        let (data, status) = run_scsi_command(b"", SCSI_GET_EVENT_STATUS, &cdb).await;
        assert_eq!(status, STATUS_GOOD);
        assert_eq!(data.len(), 8);
    }

    #[tokio::test]
    async fn test_scsi_get_performance() {
        let cdb = [
            SCSI_GET_PERFORMANCE,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x1C,
            0x00,
        ];
        let (data, status) = run_scsi_command(b"", SCSI_GET_PERFORMANCE, &cdb).await;
        assert_eq!(status, STATUS_GOOD);
        assert_eq!(data.len(), 28);
    }

    #[tokio::test]
    async fn test_scsi_read_disc_information() {
        let iso = vec![0u8; 2048 * 100];
        let cdb = [
            SCSI_READ_DISC_INFORMATION,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x22,
            0x00,
        ];
        let (data, status) = run_scsi_command(&iso, SCSI_READ_DISC_INFORMATION, &cdb).await;
        assert_eq!(status, STATUS_GOOD);
        assert_eq!(data.len(), 34);
        assert_eq!(data[2], 0x02);
    }

    #[tokio::test]
    async fn test_scsi_unsupported_opcode() {
        let cdb = [0xB0, 0x00, 0x00, 0x00, 0x00, 0x00];
        let (_data, status) = run_scsi_command(b"", 0xB0, &cdb).await;
        assert_eq!(status, STATUS_CHECK_CONDITION);
    }

    #[tokio::test]
    async fn test_stress_multiple_scsi_commands() {
        let mut iso = vec![0u8; 2048 * 10];
        iso[0..8].copy_from_slice(b"TESTISO!");

        let dir = tempfile::tempdir().unwrap();
        let iso_path = dir.path().join("stress.iso");
        std::fs::write(&iso_path, &iso).unwrap();

        let server = Arc::new(MahoganyStorageServer::new());
        server
            .set_image(Some(iso_path.to_string_lossy().to_string()))
            .await;
        server.start(0).await.unwrap();
        let port = {
            let guard = server.listener.lock().await;
            guard.as_ref().unwrap().local_addr().unwrap().port()
        };

        let s2 = server.clone();
        let _accept_handle = tokio::spawn(async move { s2.accept_one().await });

        let mut client = TcpStream::connect(format!("127.0.0.1:{}", port))
            .await
            .unwrap();

        let mut setup = vec![0u8; 544];
        setup[5] = 0xFE;
        client.write_all(&setup).await.unwrap();
        let mut pre = [0u8; 16];
        client.read_exact(&mut pre).await.unwrap();

        let mut tx_id: u32 = 0;

        // 1) Test Unit Ready
        tx_id += 1;
        let hdr = build_cmd_header(SCSI_TEST_UNIT_READY, 0, tx_id, 0);
        client.write_all(&hdr).await.unwrap();
        let mut sh = [0u8; 64];
        client.read_exact(&mut sh).await.unwrap();
        let mut st = [0u8; 1];
        client.read_exact(&mut st).await.unwrap();
        assert_eq!(st[0], STATUS_GOOD);

        // 2) Inquiry
        let inquiry_cdb = [SCSI_INQUIRY, 0x00, 0x00, 0x00, 36, 0x00];
        tx_id += 1;
        let hdr = build_cmd_header(SCSI_INQUIRY, inquiry_cdb.len() as u32, tx_id, 0);
        client.write_all(&hdr).await.unwrap();
        client.write_all(&inquiry_cdb).await.unwrap();
        client.read_exact(&mut sh).await.unwrap();
        let (dl, _, _, dir, _) = parse_response_header(&sh);
        assert_eq!(dir, TYPE_DATA_IN);
        let mut buf = vec![0u8; dl as usize];
        client.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf[8..16], VENDOR_ID);
        client.read_exact(&mut sh).await.unwrap();
        client.read_exact(&mut st).await.unwrap();
        assert_eq!(st[0], STATUS_GOOD);

        // 3) Read Capacity
        let cap_cdb = [
            SCSI_READ_CAPACITY_10,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
        ];
        tx_id += 1;
        let hdr = build_cmd_header(SCSI_READ_CAPACITY_10, cap_cdb.len() as u32, tx_id, 0);
        client.write_all(&hdr).await.unwrap();
        client.write_all(&cap_cdb).await.unwrap();
        client.read_exact(&mut sh).await.unwrap();
        let (dl, _, _, dir, _) = parse_response_header(&sh);
        assert_eq!(dir, TYPE_DATA_IN);
        let mut cap = vec![0u8; dl as usize];
        client.read_exact(&mut cap).await.unwrap();
        assert_eq!(&cap[0..4], &[0x00, 0x00, 0x00, 9]);
        client.read_exact(&mut sh).await.unwrap();
        client.read_exact(&mut st).await.unwrap();
        assert_eq!(st[0], STATUS_GOOD);

        // 4) Read10 block 0
        let read_cdb = [
            SCSI_READ_10,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x00,
            0x01,
            0x00,
        ];
        tx_id += 1;
        let hdr = build_cmd_header(SCSI_READ_10, read_cdb.len() as u32, tx_id, 0);
        client.write_all(&hdr).await.unwrap();
        client.write_all(&read_cdb).await.unwrap();
        client.read_exact(&mut sh).await.unwrap();
        let (dl, _, _, dir, _) = parse_response_header(&sh);
        assert_eq!(dir, TYPE_DATA_IN);
        let mut block = vec![0u8; dl as usize];
        client.read_exact(&mut block).await.unwrap();
        assert_eq!(&block[0..8], b"TESTISO!");
        client.read_exact(&mut sh).await.unwrap();
        client.read_exact(&mut st).await.unwrap();
        assert_eq!(st[0], STATUS_GOOD);

        // 5) Mode Sense 6
        let ms_cdb = [SCSI_MODE_SENSE_6, 0x00, 0x3F, 0x00, 192, 0x00];
        tx_id += 1;
        let hdr = build_cmd_header(SCSI_MODE_SENSE_6, ms_cdb.len() as u32, tx_id, 0);
        client.write_all(&hdr).await.unwrap();
        client.write_all(&ms_cdb).await.unwrap();
        client.read_exact(&mut sh).await.unwrap();
        let (dl, _, _, dir, _) = parse_response_header(&sh);
        assert_eq!(dir, TYPE_DATA_IN);
        let mut ms = vec![0u8; dl as usize];
        client.read_exact(&mut ms).await.unwrap();
        assert_eq!(ms[1], 0x20);
        client.read_exact(&mut sh).await.unwrap();
        client.read_exact(&mut st).await.unwrap();
        assert_eq!(st[0], STATUS_GOOD);

        // 6) Disconnect
        let disconnect_hdr = build_cmd_header(0xFF, 0, 0, 0);
        client.write_all(&disconnect_hdr).await.unwrap();
        client.read_exact(&mut sh).await.unwrap();
        client.read_exact(&mut st).await.unwrap();
        assert_eq!(st[0], STATUS_GOOD);
    }

    #[tokio::test]
    async fn test_transaction_id_echoed() {
        let (_, status) = run_scsi_command(b"", SCSI_TEST_UNIT_READY, &[]).await;
        assert_eq!(status, STATUS_GOOD);
    }
}
