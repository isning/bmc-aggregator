//! Dell/Avocent legacy Virtual Media (AVMP) backend.
//!
//! This protocol is intentionally separate from the AVCT console backend:
//! Virtual Media uses the iDRAC KVM endpoint (normally TCP port 5900) and an
//! `AVMP` record layer after APCP/TLS
//! negotiation.  The wire codecs in this module are derived from Dell's
//! unstripped `libVMAPI_DLL.so`; they must not be reused by generic MSD code.

pub mod msd;
