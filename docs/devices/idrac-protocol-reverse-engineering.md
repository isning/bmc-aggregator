# iDRAC native-protocol reverse-engineering record

Status: active evidence record. This document is the prerequisite for changes
to `idrac-avct-console` and `idrac-legacy-media`; no row marked **Open** may
be implemented by inference.

## Scope and evidence policy

This records the Dell/Avocent transports used by the native backends only:

- AVCT console: APCP setup, TLS, account login, video, and HID.
- AVMP legacy virtual media: APCP setup, TLS, media request/response.

It does not describe iDRAC Web, Redfish, IPMI, or application auth.

## Boot-entry evidence boundary

Concrete firmware boot entries are a management-plane capability, separate from
the AVCT console and AVMP media protocols. Newer Redfish implementations may
return standard `BootOptions` with `UefiDevicePath`. The tested iDRAC8 instead
publishes `Links.Oem.DELL.BootOrder`, leading to a Dell BootSources resource
whose device registry defines `UefiBootSeq` and a writable `OneTimeBootMode`
ordered list. Dell WS-MAN exposes `DCIM_BootSourceSetting` on supported
iDRAC6/8 firmware with `InstanceID`, `BIOSBootString`, enabled state, and
current/pending order fields. `DCIM_BootConfigSetting` separately publishes
the `OneTime` configuration and its `IsNext` state. OpenStack's
`python-dracclient` provides an independently reviewable WS-MAN invocation
contract for `ChangeBootOrderByInstanceID`: the BootConfigSetting InstanceID
is the selector and one or more complete BootSourceSetting InstanceIDs are
repeated `source` inputs. The implementation uses selector `OneTime` with one
enabled source from Dell's published IPL, BCV, or UEFI families, requires Dell
`ReturnValue=0`, and does not alter any persistent boot-order configuration.

Hardware validation on the updated iDRAC6 and iDRAC8 returned
`BOOT001`/`ReturnValue=0` and exposed the `OneTime` configuration as next after
selecting real entries. The iDRAC6 BIOS schema publishes `OneTimeBootSeq`,
`OneTimeHddSeq`, and `OneTimeUefiBootSeq` as
`OneTimeBootMode.PossibleValues`, with distinct `OneTimeBootSeqDev`,
`OneTimeHddSeqDev`, and `OneTimeUefiBootSeqDev` fields;
IPL, BCV, and UEFI selections each survived an aggregator restart in the
matching field. iDRAC8 retained its UEFI selection through BootSources plus
BootConfig readback. No host reboot was issued. This management-plane method remains
separate from AVCT/AVMP; no console or virtual-media field has been assigned a
boot-entry meaning.

| Label | Meaning |
|---|---|
| **S** | Statically confirmed from the named official class/method. |
| **R** | Statically serialised by calling an official message class with non-secret default values. |
| **N** | Statically confirmed from a named symbol in Dell's official, unstripped native VM library. |
| **H** | Hardware outcome only; it must never define a field or message meaning. |
| **Open** | Not established. The implementation must reject or leave it unsupported. |

The local reverse-engineering artifacts are ignored and never read by the
running service. The analysed clients are:

| Client | SHA-256 | Protocol namespaces |
|---|---|---|
| iDRAC8 AVCT KVM, 5.04.04 build 425 | `d67fe4206925dd8b318f50a08ae3efd06434f84d0f2aa6211110abecde71deba` | `com.avocent.c.d`, `com.avocent.d.a`, `com.avocent.d.d` |
| iDRAC6 AVCT KVM | `fbc1f0650e16fcc7fa71fbe7a14d0552df49d9d06046fa5c7a92235b3bcdb247` | `com.avocent.kvm.b.a`, `com.avocent.kvm.b`, `com.avocent.vm` |
| iDRAC8 VM native library, `libVMAPI_DLL.so` | `e33dc97ebb143f4070d1cd13edc20be1faa8e4aadc594d1b1fafabcdcf8b946e` (ELF build-id `596336c3df4d88cc648d4799fdca23976e2fe131`) | unstripped `AVMPPacket`, `ProtocolAPCP`, `AVMPSession` classes |

The two clients use different package names. Package-name equality is not
protocol evidence; only their wire encoders and parsers are used below.

## Confirmed AVCT framing

### APCP before TLS

**S** `com.avocent.d.a.a#a(byte, byte[], int)` serialises the modern,
68-byte APCP request:

```text
0..3   "APCP"
4..7   total length: u32be = 68
8..9   record type: u16be = 0x0101
10..11 requested APCP version: branch-dependent
12     channel: 3 (control) or 4 (video)
13..14 protocol major/minor
15     reserved = 0
16..19 channel profile / port selector: u32be
20     random length = 32
21..52 random bytes
53..67 branch-dependent reconnect/session fields
```

**S** `ProtocolAPCP#a(byte, Socket, int)` sets the video selector to `1541`
before serialising channel 4. The control selector is obtained from APCP
session state and must not be guessed from a local feature name.

**S** the server record starts `APCP`, then has a u32be total length,
u16be marker (`0x8100` or `0x8101`), a u16be negotiated version, protocol
major/minor, transport bits, redirect port, and an opaque random field.

**S** `com.avocent.d.a.d#run` writes `APCP || u32be(12) || u16be(0x0400) ||
u16be(0)` every ten seconds when its APCP keepalive thread is active. It is
not an AVCT `BEEF 0x0400` packet.

### AVCT after TLS

**S/R** both releases use an 8-byte header for normal AVCT packets:

```text
0..3  "BEEF"
4..5  message type, u16be
6..7  total packet length, u16be, including this header
8..   payload
```

Display-channel authorisation is the exception. **R** iDRAC8 `ic` and iDRAC6
`yb` produce `00 00 00 00 | 01 01 | 00 10` followed by two u32be values. The
first is the client random from the preceding login; the second is the
appliance random. It is a video-channel attach record, not a second account
login.

### Account-login version selection

**S** iDRAC8 `com.avocent.c.d.ib(int major, int minor)` selects the login
encoder from the negotiated APCP profile:

| Negotiated profile | AVCT type | Encoder |
|---|---:|---|
| profile 1 | `0x0100` | legacy fixed account fields |
| profile 2 | `0x0102` | fixed 217-byte packet including the outer header |
| profile 3 (`major >= 2 && minor >= 41`) | `0x0104` | variable-length modern account packet |

The iDRAC8 session defaults to `(2, 41)` and accepts a newer server pair only
when it is not lower. A fixed `0x0102` implementation is therefore not
protocol-aligned for the normal iDRAC8 branch.

**S** iDRAC6 `com.avocent.kvm.b.a.y(boolean)` uses its own fixed
`0x0100`/`0x0102` layouts. It does not prove that iDRAC8 `0x0104` is accepted
by iDRAC6.

The account fields are bounded and the ticket/RIP token is a distinct
eight-byte field. Direct Web-account login leaves that token empty; it must
not be exposed in diagnostics or reinterpreted as a VNC password.

### Exact iDRAC8 login wire layouts

**R** The local `DumpLoginLayouts` helper invokes only `ib`'s constructor and
serializer with public, non-secret fixture values (`"u"`, `"p"`, random
`0x01020304`). Its output agrees with the source of
`com.avocent.c.d.ib#o()`. All integers below are big endian.

| Profile | Type | Header length | Payload layout |
|---|---:|---:|---|
| `1.x` | `0x0100` | `216` | `username_len:u8`, `username:[96]`, `password_len:u8`, `password:[96]`, `ticket:[8]`, `selector:u8`, `channel:u8`, `client_random:u32` |
| `2.0..2.40` | `0x0102` | `217` | profile-1 layout plus final `mode:u8` |
| `>=2.41` | `0x0104` | `31 + username_len + password_len` | `username_len:u16`, `derived_1:u16`, `password_len:u16`, `derived_2:u16`, `ticket:[8]`, `selector:u8`, `channel:u8`, `client_random:u32`, `mode:u8`, `username:[N]`, `password:[M]` |

For profile 3, the two derived values are exactly
`derived_1 = 31` and `derived_2 = 31 + username_len`; this surprising order
is the official encoder's order, not a reinterpretation. The direct console invocation calls
`d(1)`, `a(0)`, `f(client_random)`, and `e(mode)` before serialization. The
ticket is a separate eight-byte input that is all zero only when the official
launcher has no ticket/RIP value.

No profile accepts an unbounded account field: profile 1/2 uses two 96-byte
fixed regions; profile 3 length fields are `u16`, but the client obtains the
strings from its launcher. The Rust backend must retain a safe explicit bound
rather than treating the absence of a fixed field as permission to allocate
arbitrarily.

### Login-status response layouts and status space

**S** `com.avocent.d.d.a.c#a(byte[], byte[])` and the iDRAC6-equivalent
`com.avocent.kvm.b.a.tb#a(byte[], byte[])` parse two server variants. The
packet type selects the variant; the fixed fields below are byte offsets in the
payload.

| Type | Bytes | Layout |
|---:|---:|---|
| `0x8300` | at least 9 | `status:u8`, bytes `1..2` reserved by this parser, `flags:u8` at 3, `server_value:u32` at 4, `name_len:u8` at 8, `name:[name_len]` at 9 |
| `0x8305` | at least 11 | `status:i8` at 0, `status_flags:u8` at 1, `timeout_or_mode:u8` at 2, byte 3 skipped, `server_value:u32` at 4, `flags:u16` at 8, `name_len:u8` at 10, `name:[name_len]` at 11 |

The iDRAC8 response parser consumes these flag bits exactly:

| Field | Bit | Official effect |
|---|---:|---|
| `flags` (`0x8300` byte 3 / `0x8305` u16 at 8) | `0x0001` | Selects `C()` rather than `D()` after successful login. |
| same | `0x0002` | Stored by the session before video start; its product name is not established by the parsed class. |
| `0x8305` flags | `0x0020` | `j()` is true; activates an optional post-login branch. |
| same | `0x0040` | `i()` is true; only read together with the preceding bit. |
| same | `0x0080` | `k()` is true; stored for an optional session branch. |
| same | `0x0100` | `l()` is true; stored for an optional session branch. |
| `0x8305` status_flags | `0x01` | `v()` is true; it selects a documented error-text family only. |

The numeric login status is not a three-value enum. **S** iDRAC6
`AVMPStatus` declares the shared console status vocabulary: `0` success;
`1` invalid user; `2` invalid password; `3` channel access denied; `4` channel
in use; `5` channel not found; `6..8` server/channel unavailable variants;
`9` insufficient rights; `10` locked out; `11..23` preemption, exclusive-use,
or certificate/network-auth variants; `50..54` preemption timeout variants;
`60..64` shared/exclusive/preemption limits; and `100` network failure. A
client must preserve the numeric status for diagnostics and map every known
non-zero status to a terminal feature fault. It must not collapse unknown
non-zero statuses into successful login.

### APCP request variants and server result

The official `ProtocolAPCP` has three distinct writers. They must remain
distinct in the implementation:

| Writer | Size | Outer type/version | Tail after 32-byte random |
|---|---:|---|---|
| `a(byte, byte[])` | 53 | type `0x0100`, version `0x0104` | none |
| `a(byte, byte[], boolean, int)` | 68 | type `0x0100`, version `0x0104` | `preference:u16`, `0:u8`, `180:u16`, `30:u16`, `session:u32`, `0:u32` |
| `a(byte, byte[], int)` | 68 | type `0x0101`, version `0x0000` or `0x0104` selected by official configuration | `0:u8`, `0:u16`, `0:u32`, `session:u32`, `0:u32` |

All three share bytes `12..52`: channel, protocol major/minor, zero byte,
u32 selector, random-length byte, and a padded 32-byte random field. The
modern video path assigns selector `1541` immediately before calling its
68-byte writer. It passes a session value supplied by the session object; zero
is a valid initial value but is not a universal reconnect value.

The supplied iDRAC8 Java initial-control overload `ProtocolAPCP#b()` has the
following exact writer selection. These are launcher preferences, not values
learned from the APCP response:

| `CAPABILITY_RECONNECT` | `RECONNECT_VERSION` | Directly reachable initial writer | Source consequence |
|---|---:|---|---|
| not `ENABLE` | any | 53-byte `0x0100` | parsed through `a(Socket, 0)` |
| `ENABLE` | `0` | 68-byte `0x0101`, profile `0x0000`, session `0` | parsed through `a(Socket, 101)`; the same writer is later used with retained `Fb` by reconnect |
| `ENABLE` | any value other than `-1` or `0` | 68-byte preference `0x0100` | the directly reachable supplied branch uses the boolean receiver; its private alternate 53-byte branch is never assigned true in the artifact |
| `ENABLE` | `-1` | no connection writer selected by this overload | unsupported launcher configuration |

The explicit reconnect overload still sends 68-byte `0x0101` with retained
`Fb` after it has passed the session's reconnect gate. Thus `0x0101` plus a
zero session field is not enough to infer whether a peer is reconnecting;
the local typed launcher/session state is required.

**S** The server parser first reads:

```text
APCP | total_len:u32 | marker:u16 | version:u16 | major:u8 | minor:u8 |
transport_bits:u32 | redirect_port:u16 | random_len:u8 | random:[N] | padding
```

`marker` is exactly `0x8101` or `0x8100`; transport choice is a bit field, in
which the official client prefers `0x00000001`, then `0x00000004`, then
`0x00000008`. It follows a non-zero `redirect_port` before TLS. The remaining
record bytes are conditional on marker, negotiated version, and transport
bits. Their exact branch shapes and the few source-visible side effects are
recorded in [APCP server tails](#apcp-server-tails-exact-branch-boundaries-opaque-fields)
below. Scalars without a source-visible product meaning remain raw fields;
that is deliberately different from treating the whole tail as unknown.

## Shared AVCT message inventory

These rows are **S/R** in both JAR families. Names are returned by the
official message classes, not guessed product labels.

| Type | Direction | Official name | Confirmed payload contract |
|---:|---|---|---|
| `0x0000` | client | Video Ack | `counter:u8` then seven zero bytes; default batching emits counter `1`, while non-default batching is stateful. |
| `0x0001` | client | Video Channel Auth | Two u32be random values; outer magic is four zero bytes. |
| `0x0200` | client | KeyboardDataRequest | 8-byte HID payload. |
| `0x0201` | client | Mouse Data Request | 8-byte HID payload. |
| `0x0202` | client | MouseOrigin | 8-byte payload. |
| `0x0203` | client | Mouse Reset | 8-byte payload. |
| `0x0204` | client | Keyboard LED Request | 8-byte payload. |
| `0x0208` | client | FocusControl | 8-byte payload. |
| `0x0209` | client | Mouse Delta Request | 8-byte payload. |
| `0x020b` | client | MouseAbsSyncRequest | byte 0 defaults enabled; byte 1 is a separately settable boolean that defaults zero; bytes `2..7` are zero. |
| `0x0301` | client | ScreenRefresh | Exactly `BEEF 03 01 00 10` plus eight zero bytes. |
| `0x0302` | client | Set Display Area | two u16be dimensions followed by four zero bytes. |
| `0x0304` | client | SetScaleMode1to1 | eight zero bytes. |
| `0x0306` | client | AutoVideoAdjust | eight zero bytes. |
| `0x030e` | client | Video Enable Request | normal `true` constructor emits `01 01 00 00 00 00 00 00`. |
| `0x0400` | client | KeepAlive | AVCT keepalive, distinct from APCP `0x0400`. |
| `0x0403` | client | SetDVCColorDepthMessage | four meaningful control bytes; see colour-depth section. |
| `0x0084` | server | Video Connect Status | byte 0 zero means start-video in the official response handler. |
| `0x8200` | server | Input Resolution Response | width, height, and display-format data. |
| `0x8201` | server | Display Resolution Response | two u16be dimensions; official code updates state/UI and sends no ACK. |
| `0x8202` | server | VideoSetupData | 24-byte payload containing ten u16be fields. |
| `0x8204` | server | DVCColorModeResponse | colour-mode/depth status. |

`ScreenRefresh` is therefore not the current uncertainty: its type, header,
payload, writer, and legacy/iDRAC8 class lineage are all confirmed.

## Console state machine and identified implementation deltas

**S** the iDRAC8 control response class is `com.avocent.d.d.a.c`. It parses
`0x8300` and `0x8305` login-response variants with status, flags, port/session
fields, and a bounded server string.

**S** `com.avocent.d.g.a.b#a(loginResponse, ...)` reacts to a successful login
by selecting a primary/secondary path, then calling `AVSPKVMSession.D()` or
`AVSPKVMSession.C()`. `D()` opens the video APCP/TLS socket and immediately
queues Video Channel Auth. The official source does not use the later
`0x8302 -> 0x8430 -> 0x8102 -> 0x8410` messages as a required attach barrier.

**S** `AVSPKVMSession.C()` always sends `0x030e` after `0x0084`, but selects
the next step from negotiated capability and local scaling state:

| Official condition | Follow-up after `0x030e` |
|---|---|
| legacy-cert video setting | `0x0302` using certificate dimensions |
| RIP connection with no stored geometry | `0x0304` only |
| RIP connection with stored geometry | `0x0302` or `0x0304`, selected from full-scale/autoscale settings |
| non-RIP fallback | `0x0302(1024,768)` then `0x0304` |

The iDRAC6 `AVSPKVMSession.t()` triple is legacy-only evidence. It does not
authorize unconditional `0x030e -> 0x0302 -> 0x0304` on iDRAC8.

The following APCP records exist but their current unconditional post-video-
attach use is contradicted by official call sites:

| Record | Official producer and trigger | Rule |
|---|---|---|
| 68-byte `MgmtSessionRequest` (`com.avocent.c.f.c`) | `AVSPKVMSession.qb()`, only after the NextBoot capability path | Send only after parsing that capability. |
| APCP `0x0102` (`com.avocent.c.f.f`) | response-handler reconnect/session-id path | Never treat as generic post-attach sync. |
| 48-byte APCP `0x0103` (`com.avocent.c.f.e`) | response-handler OTP/reconnect path | Never treat as generic display profile. |
| APCP `0x01ff` (`com.avocent.c.f.a`) | normal close, written to both writers | Use only for normal session close. |

### Confirmed control/video state transitions

The official code establishes this order for the direct, separate-video-socket
path. This is a state machine, not an observed timing trace.

```text
APCP(control) -> TLS(control) -> version-selected LoginRequest
  -> LoginResponse(status == 0)
  -> response flag bit 0 clear: D()
  -> APCP(video) -> TLS(video) -> VideoChannelAuth(client_random, appliance_random)
  -> VideoConnectStatus(0x0084) -> C() -> VideoEnableRequest(0x030e)
  -> branch-selected scale/display request -> display packets
```

`D()` is `com.avocent.d.d.b#D`; it opens the video socket and sends `ic` (the
non-`BEEF` channel-auth record) immediately. `C()` is
`com.avocent.d.d.b#C`; it writes `kb(true)` (`0x030e`) first and then chooses
the geometry/scale action. The `0x0084` handler only invokes `C()` directly
when the two official local mode predicates are both false. In other official
branches, `C()` is invoked after resolution/setup handling. Therefore
`0x0084` is a state input, not permission to unconditionally emit all three
legacy bootstrap packets.

The inverse conditions are also explicit:

- login `status != 0` never opens video;
- login flag bit `0x0001` set chooses `C()` rather than `D()`;
- `D()` itself does not wait for `0x8302`, `0x8430`, `0x8102`, or `0x8410`;
- `C()` is the only path in this source that writes `0x030e`.

`0x0084` is parsed by `com.avocent.c.d.fc`: payload byte zero is `0` when the
client's `j` flag is true. The handler does not inspect the other seven bytes.
They remain opaque; a new backend must validate the documented minimum
payload length before it reads byte zero.

### AVCT normal-close record

The console's APCP close is distinct from the AVMP VM `SessionClose` below.
**S** `com.avocent.c.f.a` serialises a fixed 16-byte record:

```text
APCP | u32be(16) | u16be(0x01ff) | u16be(0) | u32be(0)
```

Although its constructor receives a session-side integer, this serializer
does not write that value. `AVSPKVMSession.ob()` queues the same record to
both of its control/video writers when they exist. A native AVCT close must
therefore send this exact console close record only on the normal-close
transition, drain the relevant writer queues, and then close sockets. It must
not substitute the VM `SessionClose` implementation, and it must not send
an attach/reconnect record during teardown.

### AVCT console reconnect: separate Java state machine

The iDRAC8 AVCT console and the native VM engine do not share reconnect
control flow. This is **S** from the official Java classes
`com.avocent.d.d.b`, `com.avocent.d.d.{o,q,s,t}`, `com.avocent.d.a.a`, and
`com.avocent.d.g.a.a`; it is deliberately separate from the AVMP state
machine below.

Every successfully decoded inbound AVCT/APCP record calls
`com.avocent.d.d.b::mb()`, incrementing the console's received-packet
counter. The post-login response handler starts `com.avocent.d.d.o`, whose
default loop interval is `1100 ms` and whose missed-counter threshold defaults
to `5` (the threshold is read from a local preference). It compares the
counter with its last observed value on each iteration; when the value remains
unchanged for more than that threshold and the console is not already in its
guarded reconnect state, it calls `b::Z()`.

`b::Z()` marks the console's internal connection-state property as its
reconnect state, enables the connection transition, and schedules one
one-shot `TimerTask` (`com.avocent.d.d.s`) for exactly `60000 ms`. The task
runs `b::G()` and then `b::ib()`. A reader returning no packet can instead
invoke `G()` and `ib()` directly through `com.avocent.d.d.q`; the delay is
therefore not a universal timeout for every disconnect path.

`b::ib()` refuses the terminal state, otherwise enables its reconnect
transition, notifies its registered console listeners, and calls `b::jb()`.
`jb()` cancels the prior reconnect-delay timer, builds a fresh
`com.avocent.d.a.a` APCP helper using the retained host, port, negotiated
version floor, TLS trust/cipher configuration, and saved reconnect value
`Fb`, and schedules a separate one-shot `com.avocent.d.d.t` task for
`180000 ms`. That 180-second task belongs only to this Java-console recovery
workflow; it is not an AVMP timer and not a BMC wire timeout field.

The precise reconnect request branch is confirmed by
`b::jb() -> a::b(Fb) -> a::a(byte=3, Socket=null, int=Fb)`:

```text
fresh TCP socket
APCP SessionRequestEx writer
  channel/protocol argument = 3
  selector field set to 1541 by this branch
  APCP type = 0x0101
  session field = saved Fb argument
APCP response parsing and its selected TLS/socket path
```

This is the evidence that ties the Java console's `0x0101` request with a
retained non-zero `Fb` field to its reconnect branch. It does *not* make
`0x0101` an initial connection default, nor does it permit reusing a
hard-coded zero tail. The ordinary initial `a::b()` path is a different
overload and chooses among the documented APCP writers from its
launcher/configuration predicates. In particular, the supplied Java source
also selects the same 68-byte `0x0101` writer with `session = 0` during an
initial connection when `CAPABILITY_RECONNECT=ENABLE` and
`RECONNECT_VERSION=0`; see the initial-writer table below. Message type alone
therefore cannot identify a reconnect.

After the APCP helper returns a non-null control socket, `jb()` installs the
control reader/writer and invokes `b::b(control_port, control_channel, false)`.
That method creates the normal version-selected `ib` account-login packet,
loads the retained console username/password fields, regenerates the client
random, and explicitly supplies the false preempt argument. The official
console therefore re-authenticates after a reconnect APCP exchange; it does
not reuse a VNC password or skip directly to video frames. The existing
login-response handler then follows the separately documented attach path to
open/authorise the display channel.

Connection exceptions in `jb()` retry the APCP helper recursively after two
five-second sleeps while no control socket exists. A distinguished official
APCP exception sets the console's internal failure property and the `fc`
guard; the 180-second task also sets that guard when its own
connection-state predicate still permits it. These are Java-client/UI state
transitions, not wire-level login statuses. The decompiled listener source
identifies public notifications for `VIDEO_SESSION_RECONNECTING`,
`VIDEO_SESSION_RECONNECT_FAILED`, and `VIDEO_SESSION_LINK_INTERRUPTED`, but
does not provide a stable BMC numeric error packet for all local timeout
branches. A backend must therefore report its own structured transport fault
for those terminal local failures; it must not leave a video request pending
while attempting this recovery loop.

#### Console reconnect teardown: exact `G()` boundary

**S** `com.avocent.d.d.b#G()` is the common teardown called by the delayed
reconnect task and by terminal `H()`. It first returns without side effects
when the internal connection property is already the terminal value. On every
other path it performs the following ordered cleanup before a subsequent
`ib()` can create a new APCP helper:

1. Stops the optional `Mb` helper; if the session is in its special local
   mode, it invokes that mode's reset with zero.
2. Stops and nulls the `ec`, `z`, `Yb`, `G`, and `M` worker/helper fields,
   resetting `Lb` to zero with `ec`.
3. If the playback/session object is live, invokes its stop method.
4. Closes and clears the control socket `A`, then closes and clears the video
   socket `B`.
5. Stops and clears the control/video writers `U`, `S`, and `T`; then closes
   the control and video input streams `D` and `F`.

This is source order, not a suggested cleanup order. In particular, a native
replacement must not start a fresh reconnect over an old control or video
socket, and a user-initiated normal close must send the already documented
`0x01ff` record before entering this local resource teardown. `H()` is the
terminal wrapper: it calls `G()` and then invokes the session's terminal
handler. The symbolic property strings used by `Z()`, `ib()`, and `H()` remain
an unrecovered Java-UI vocabulary; they are not wire states and are not
needed to reproduce the verified close/reconnect socket boundary.

### Confirmed video setup and resolution records

| Type | Official class | Fixed payload parser | Client reaction |
|---:|---|---|---|
| `0x8200` | `tb` | `width:u16` at 0, `height:u16` at 2, format tag:u32 at 4 | stores size/tag; tag values `0x4153`, `0x4435`, `0x5033` map to internal selectors `2`, `1`, `3`; the literal `P3` additionally sets a boolean. |
| `0x8201` | `qb` | width/height u16 at 0/2 | updates visible resolution; no reply is written by its handler. |
| `0x8202` | `gc` | ten u16 values at offsets `0,2,...,18` | stores all ten values; no reply is written by its handler. |
| `0x8204` | `q` | `mode:u8`, `flag_1:bool`, `flag_2:bool` at 0/1/2 | reports DVC state only; no reply is written by its handler. |

`0x8206` is not an invented record: the iDRAC8 receive factory sends both
`0x8200` and `0x8206` through the same `tb` parser. A backend must therefore
accept `0x8200` as the canonical Input Resolution Response and may accept
`0x8206` only as the source-confirmed alias. Neither type authorises a reply
by itself. Treating `0x8206` as a substitute for `0x8200`, or treating either
type as a completed display attach, is a static-source mismatch.

### Video acknowledgement scope

**S** The `0x0000` acknowledgement object is `com.avocent.c.d.jb`. It
serialises `counter:u8` followed by seven zero bytes. `AVSPKVMSession.I()`
increments its `Q` counter, sends an acknowledgement when `Q >= R`, and then
resets `Q` to zero. The reference session initializes `R = 1`, so the direct
default is exactly `[1, 0, 0, 0, 0, 0, 0, 0]`. A non-default batching value or
an incremental-ack mode changes the first byte; no implementation may call it
a constant outside the default branch.

## Display, HID, colour depth, and AVMP

**S** the common display envelope accepts `0x0081`, `0x0082`, `0x0083`,
`0x008a` and high variants `0x8601`, `0x8602`, `0x8603`, `0x860a`. It reads
width/height at payload offsets 4/6, flags at offset 8, and compressed data at
offset 12. The official renderer maps these to four codec selectors. All four
selector algorithms are ported from the official implementation; generation
profiles may still restrict which control combinations they advertise when
the firmware's selected wire family and payload shape are not interoperable.

**S** `0x0403` contains colour-mode boolean, target depth, slider variant,
and a final mode boolean in its first four bytes. `0x8204` is its response;
it is not an HID authorisation response.

**S** iDRAC6 `PacketAVMP` confirms the readonly-media message constants:
`0x0200` enumerate drives, `0x0210` map, `0x0220` release, `0x0300` return
read bytes, `0x8200` drive information, `0x8300` appliance read request, and
`0x8410` device status.  The iDRAC8 native parser boundaries for writable
drive, alternate-TOC, and configuration-option records are recorded below;
their sender/feature flows remain unsupported. Readonly floppy mapping is
supported through the generation-specific drive types and shared `0x0211`
geometry request recorded below. Smart-card runtime remains unsupported.

### Display-family map and common envelope

**S** `com.avocent.d.g.a.a` dispatches exactly eight display message types to
the common `com.avocent.kvm.a.a.j` envelope:

| Wire types | Internal selector | Official enum name | Decoder family |
|---|---:|---|---|
| `0x0081`, `0x8601` | 0 | `DVC15` | stateful RLE, 15-bit literal renderer |
| `0x0082`, `0x8602` | 1 | `DVC7` | stateful RLE, 7-bit renderer |
| `0x0083`, `0x8603` | 2 | `DVC7_GRAY` | stateful RLE, grayscale renderer |
| `0x008a`, `0x860a` | 3 | `DVC23` | stateful RLE, 23-bit literal renderer |

For all eight types `j#a` uses this wire envelope:

```text
0..3    codec/reserved bytes retained by the message object
4..5    first u16 dimension
6..7    second u16 dimension
8       flags: bit 0 and bit 1 are consumed by the renderer
9..11   renderer-specific header bytes
12..    compressed display data
```

The official wrapper passes the second dimension as its first renderer
dimension and the first dimension as its second one. That is an observed
calling convention, not a basis for renaming bytes "width" and "height" in a
protocol codec. The Rust adapter must keep both raw dimensions until the
specific decoder says which is horizontal. It must also reject zero,
overflowing, and unsupported selector values with a feature fault.

The selector implementations are source ports and are covered by official
input/output fixtures. The `0x86xx` variants reuse the corresponding selector;
they are known wire types rather than unknown messages. A source must still
fail explicitly if its payload violates that selected command reader instead
of remaining in `Waiting for first frame` or trying a different codec.

The table above is the official client's generic type dispatch. The recorded
iDRAC6 firmware has one hardware-confirmed exception: after selecting base
4/5, it changes the envelope to `0x0083` but continues emitting the RGB555
DVC15 command grammar used by `0x0081`. The first 6506 compressed bytes of a
captured `0x0083` chain and the preceding `0x0081` chain are byte-identical
(`SHA-256 dd05644d1bda34fe4e06668f873f781bac8eef80a422547aa502ea9d928c9605`).
The official GRAY7 decoder and the Rust GRAY7 port therefore produce the same
striped image, while the official DVC15 decoder produces the complete 1024x768
console from that exact `0x0083` payload. The decoder selects this dialect from
the explicit iDRAC6 transport profile; it does not inspect image content and
does not change the iDRAC8 type dispatch.

### DVC decoder core (all four selector families)

The source needed to port the decoder is available in the official Java
client; it is not an opaque/native rendering path. **S**
`com.avocent.d.e.a.c#run` selects the implementation below from the envelope
selector: `0 -> kvm.a.a.d`, `1 -> kvm.a.a.g`, `2 -> kvm.a.a.f`, and
`3 -> kvm.a.a.e`. These implementations share the command reader
`kvm.a.a.c` and the ARGB framebuffer `b.d.c.b`.

The following is a literal description of the reader's operations, expressed
in terms of its framebuffer cursor. It deliberately does not assign invented
names to the command classes.

| First-byte predicate | Official operation (`kvm.a.a.c`) |
|---|---|
| `byte & 0xe0 == 0x00` | Advance the framebuffer cursor by the decoded run length; pixels in that interval are retained. |
| `byte & 0xe0 == 0x40` | Copy the decoded run from `cursor - framebuffer_width`; if that source position is negative, do not write a substitute value. |
| `byte & 0xe0 == 0x20` | Fill the decoded run with the immediately preceding framebuffer pixel; cursor zero uses the framebuffer's guarded zero-value lookup. |
| `byte & 0xe0 == 0x60` | Emit four pixels selected from the preceding pixel and the nearest preceding different pixel; when bit `0x10` is set, consume zero or more following 7-pixel selector groups until a group has bit `0x80` clear. |
| `byte & 0x80 != 0` | Consume a literal through the selector-specific method below. |

For the skip/previous-pixel/previous-row commands, the run length starts with
the low five bits of the first byte. Up to four immediately following bytes
with the same high-three-bit class extend it in five-bit chunks,
least-significant group first. The reader leaves the first byte of the next
command unconsumed. DVC15 and DVC23 use that value directly; DVC7 and
DVC7_GRAY override `g()` and add two to it. The fixed four-pixel pattern
command does not use that run-length helper. This behavior is from
`kvm.a.a.c#g`, `f#g`, and `g#g`, not inferred from a packet sample.

The literal conversions are also fully static-source-confirmed:

| Selector/type family | Class | Literal bytes | ARGB conversion performed by the official renderer |
|---|---|---|---|
| `0` / DVC15 | `kvm.a.a.d` | two (`first & 0x7f`, then one byte) | Form a 15-bit value. Expand red from bits `14..10` with `<< 7`, green from `9..5` with `>> 2`, blue from `4..0` with `<< 3`, then OR `0xff000000`. |
| `1` / DVC7 | `kvm.a.a.g` | one (`first & 0x7f`) | Index the fixed 128-entry palette initialized by `kvm.a.a.a#a`; do not substitute a generic RGB332 palette. |
| `2` / DVC7_GRAY | `kvm.a.a.f` | one (`first & 0x7f`) | Shift the seven-bit value left once and copy that value to R, G, and B. |
| `3` / DVC23 | `kvm.a.a.e` | three (`first & 0x7f`, then two bytes) | Form a 23-bit value; renderer extracts red from `0x7f8000 >> 15`, green from `0x007f80 >> 7`, blue from `(0x00007f) << 1`, then ORs `0xff000000`. |

The official DVC adapter (`kvm.a.b.a.a#a(lb)`) retains three envelope flag
bits: bit 0, bit 1, and bit 2. Bit 0 and bit 1 are also exposed by the common
`j` envelope. The selected RLE decoder receives bit 0 as its first boolean;
at the start of the next decode call `kvm.a.a.c#b()` invokes `e()`, which
calls the framebuffer's no-argument update/reset hook and sets its pixel
cursor to zero. The official `int[]` framebuffer is *not* filled with black
in this path (`b#h()` is the separate fill operation). Thus bit 0 is
evidence-backed cursor/update-cycle reset behavior, not permission to invent
a full-frame clear. Bit 1 is retained by the display scheduler as a separate
boolean; bit 2 is retained by the DVC adapter. Their product semantics are
not named by the source and must remain raw flags.

The command reader itself also owns a real pending-buffer state: it can fetch
the next DVC payload while extending a run or a pattern group, but it stops
when its packet-boundary boolean demands it. A Rust port must therefore retain
decoder state **and the framebuffer** across packets, and must distinguish
“incomplete command at a permitted continuation boundary” from a malformed
truncated packet. A stateless "decode this payload into a fresh image" routine
is not equivalent. Until this state machine is covered by fixed input/output
fixtures taken from the official decoder, the relevant selector must be
terminally unsupported.

The iDRAC6 source makes the boundary condition concrete. `kvm.d.b#d()` calls
the scheduler's `f()` to obtain the next `kvm.d.i` payload when a literal,
run-length extension, or pattern selector group reaches the payload end.
`kvm.d.b#g()` uses the same path while looking ahead for another same-class
five-bit run group. It does so only when `i#l()` is false; `kvm.d.a.b#f()`
passes this from envelope flag bit 1. The Rust decoder consequently retains
only the unconsumed command bytes when bit 1 is clear, and treats an
unfinished command with bit 1 set as malformed. Regression tests cover a
DVC15 literal, run extension, and selector pattern group split between two
payloads. This is a source-port, not a heuristic packet concatenation.

The iDRAC6 fresh-session DVC15 startup policy uses these fields without
renaming them. Bit 1 is a command/payload-chain boundary, not evidence of a
video frame boundary. Bit 0 resets the retained decoder cursor. Hardware
interop showed that a persisted DVC7/DVC7_GRAY stream can begin with retained
deltas that neither the official renderer nor a fresh Rust framebuffer can
reconstruct. The aggregator therefore drains the initial selected-DVC15 chain,
sends one source-confirmed `ScreenRefresh`, waits for a reset-marked DVC15
chain, and publishes only after that chain reaches its bit-1 boundary. This is
an explicit product recovery policy built from source-confirmed primitives;
it is not presented as an undocumented Dell handshake.

The same rule is required after a runtime `0x0403` write. Hardware traces show
that the first reset-marked chain is a transition chain and can still belong
to the previous family. Accepting it completed the operation against stale
data. The corrected state machine drains that chain without decoding, sends
`ScreenRefresh (0x0301)` only at its bit-1 boundary, and accepts the next
reset-marked chain as the requested profile. Fallback to `variant-1-15` uses
the identical two-stage process.

**H/S differential result.** On the recorded iDRAC6 firmware, accepted base
profiles 4/5/6/7 stabilize on `0x0083`, `variant-1-7` stabilizes on `0x0082`,
and `variant-1-{9,12,15,18,21,23}` stabilize on `0x0081`. The official Java GRAY7
decoder and the Rust GRAY7 decoder produce the same striped layout from the
captured `0x0083` stream. Byte comparison then establishes that the appliance
did not emit GRAY7 at all: its `0x0083` payload is the same DVC15 payload sent
under `0x0081`. Decoding that payload with the official DVC15 reader restores
the complete console. This is a transport-specific wire dialect, not an image
heuristic or decoder fallback.

Hardware acceptance against the corrected two-stage state machine and the
iDRAC6 payload dialect shows clean 1024x768 frames for all eleven accepted
profiles. The `0x0082` target refresh sampled on the
static console contained only a four-byte retained-frame command, so it is not
claimed byte-identical to DVC15. Its dialect is instead established by live
differential rendering: the official DVC7 palette produces the reproduced
striped framebuffer, while RGB555 restores the correct console with zero
decode errors. Opposite-family probes prove that Max Video 4/5/6 and
Performance 9 through 23 are ignored, rather than valid same-family aliases.

On the recorded iDRAC8, `0x8204` confirms fourteen accepted profiles: base
4-7 use `0x0083`, variant 4-7 use `0x0082`, variant 9-15 use `0x0081`, and
variant 18-23 use `0x008a`. Performance 9 through 23 leaves the prior raw mode
and preference unchanged. A profile is therefore committed only after a newer
status record matches `mode == depth_bpp` and `flag_1 == variant`; the status
record remains an applied-state report rather than a wire-level request ACK.

### iDRAC6 virtual media transport gate

`com.avocent.vm.VirtualMedia#open` sends the 53-byte iDRAC6 APCP request,
then switches on the scalar `SessionSetup.connectionCapabilities`: values 1,
2, and 8 return failure immediately; only value 4 wraps the already-open TCP
socket in TLS and proceeds to AVMP authentication. This is not the iDRAC8
bit-vector priority rule. This documents the Java UI behavior, not an AVMP
wire prohibition: the raw capability-one path below is separately proven by
hardware interop and is selected only for the explicit iDRAC6 backend profile.

**Updated hardware result.** The iDRAC6 raw-AVMP slot table can instead
advertise one available slot as `type=0, capabilities=1`. A full hardware
probe sent the ordinary ISO `VDiskMap` request to index zero, received a
successful `DeviceStatus`, served two BMC `ReadRequest` blocks, and sent the
official fire-and-forget `VDiskRelease`. For this explicit iDRAC6
profile, type zero with capability bit zero set is therefore a verified ISO
mapping target; it is not synthesized or written back to the BMC.

**R** The local ignored helper `DumpDvcReference` constructs only official
in-memory iDRAC8 decoder/framebuffer objects and supplies no network data. It
provides fixed reference outputs for future Rust tests. The raw flag value `2`
is used solely because the official decoder accepts the packet boundary in
these one-command probes; no semantic label is assigned to it.

| Selector | Dimensions | Raw flags / compressed bytes | Official framebuffer output (`ARGB`) | What it fixes |
|---:|---|---|---|---|
| DVC15 | `1x1` | `2:fc00` | `fff80000` | 15-bit literal conversion |
| DVC23 | `1x1` | `2:ff0000` | `fffe0000` | 23-bit literal conversion |
| DVC7_GRAY | `2x1` | `2:0000` | `ff000000,ff000000` | the encoded zero-length skip advances two pixels |
| DVC15 | `1x1` | `3:fc00,2:00` | `fff80000` | a reset-flagged packet resets cursor/update state but does not black-fill retained pixels |

### DVC colour control is four independent fields

**S** `SetDVCColorDepthMessage` (`0x0403`, class `p`) always serialises an
eight-byte payload. Bytes `4..7` are zero in this encoder; bytes `0..3` are:

```text
0  inverse of the class's first boolean: true => 0, false => 1
1  unsigned target value
2  second boolean as 0/1
3  third boolean as 0/1
```

The display handler automatically sends a fresh `p` with its first boolean
set false when the incoming display type changes; that packet begins
`01 00 00 00`. The explicit source-control path creates `p`, sets its second
boolean according to the UI choice, sets byte 1, and sets the third boolean
from a session predicate. The official source does not attach human-readable
semantics such as “colour priority”, “extended depth”, or “interlaced” to
these three booleans. Those labels must not be exposed as fact in the UI.

`DVCColorModeResponse` (`0x8204`, class `q`) has the exact inverse parse
shape `mode:u8`, `bool:u8`, `bool:u8`, followed by unused bytes. It is status,
not an acknowledgement that a requested setting was accepted. Correct UI
behavior is therefore: offer only evidence-backed byte combinations, retain
the requested setting as pending, and report the response fields without
inventing names or claiming success from the write alone.

For iDRAC6, success is stronger than a write or `0x8204`: the operation remains
pending through the discarded transition chain and commits only after the
refreshed target chain is decoded through its bit-1 boundary. This supplies an
observable acknowledgement without assigning unsupported semantics to the
raw response booleans.

### APCP server tails: exact branch boundaries, opaque fields

The common APCP prefix is not the complete record. **S** the two receive
methods in `com.avocent.d.a.a` first consume the 53-byte prefix and then
conditionally consume the remaining fifteen bytes of a 68-byte record. The
decision inputs are the marker, negotiated APCP profile, the capability bit
`0x00000200`, and which official receive method was entered. The tail must be
stored byte-for-byte in a typed result until the corresponding branch is
implemented.

For the ordinary `a(Socket, int)` receive path, the verified branch shapes
after byte 52 are:

| Predicate | Bytes 53..67 consumed by official code | Verified behavior |
|---|---|---|
| marker `0x8101`, profile `0` | `u8, u16, u32, u32, u32` | The first `u32` must be non-zero or the official client rejects the setup. |
| marker `0x8101`, profile `>= 0x0102`, capability `0x200` set, and this is the specified video-path call | `u8, u16, u8, u16, u8, u32, u32` | The fourth scalar (`u8` at byte 59) must be non-zero or the official client rejects the setup. |
| marker `0x8101`, profile `> 100`, capability `0x200` clear | same seven-field shape | The same non-zero guard applies. |
| marker `0x8100` with capability `0x200` clear | no tail fields used by this method | The official code does **not** consume or validate any advertised suffix before proceeding to transport selection. This is a parser defect, not an alternate tail layout. |

The alternate `a(Socket, boolean)` receiver has a separate verified branch;
it is an official compatibility path, not a justification for merging the two
layouts:

| Predicate | Bytes 53..67 consumed by the alternate receiver | Verified behavior |
|---|---|---|
| marker `0x8101`, profile `0` | `u8, u16, u32, u32, u32` | The first `u32` must be non-zero or the client rejects setup. |
| marker `0x8100`, profile `>= 0x0102`, its boolean call argument true | `u8, u16, u16, u8, u8, u32, u32` | It reads the entire tail; the final `u8` must be non-zero. When capability `0x200` is present, the first `u16` is passed to a local session setter and a positive value starts the APCP keepalive helper. |

The alternate second branch has an intentionally different `u16/u8` ordering
from the ordinary video-path branch. No product field names are attached to
any of these tail scalars in this record. A native implementation must retain
the raw tail and select one parser from its own recorded request/state branch;
it must never parse “whatever looks like a 68-byte response” with one generic
tail struct. The official Java readers also ignore `total_len` and use
`read(...)` rather than `readFully(...)` for the random field. A replacement
must not copy either parser defect: before TLS starts, it must read the stated
record exactly once, bound `random_len <= 32`, require an oracle-confirmed
total of `53` or `68` for the selected branch, and reject any unmatched
marker/profile/capability/length combination as a malformed setup fault.

The AVCT Java transport chooser checks the response capability vector in this
order: `0x1`, then `0x4`, then `0x8`. Only selected value `0x4` enters the
JSSE socket-wrapping branch in this client. The individual bit labels remain
unrecovered. A backend that only implements the JSSE branch must reject a
selected `0x1` or `0x8` with a negotiated-transport feature fault, rather
than unconditionally attempting TLS.

### APCP control records after setup

The setup response is not the only incoming APCP record recognised by the
modern AVCT reader. **S** `com.avocent.d.g.a.a#a(byte[], int)` receives these
records after the eight-byte `APCP | total_len` prefix; the offsets below are
therefore relative to the remaining `total_len - 8` bytes. This table is
separate from AVCT's `BEEF 0x8100..0x8103` HID responses, which happen to
reuse the same numeric suffixes but are selected by a different magic value.

| APCP type | Official parser action | Safe implementation boundary |
|---:|---|---|
| `0x8100` | Reads `u32be@6` and constructs the internal response object `com.avocent.c.f.h` with that value. | Preserve the scalar raw. It is not an AVCT keyboard-LED response and it does not by itself authorise video start. |
| `0x8102` | Reads `u32be@4`, stores it as `AVSPKVMSession.Fb` through `b::j(int)`, and creates `com.avocent.c.f.j`. Only when the exact local parser configuration is active does it additionally read `u32be@8`. | The native backend now bounds-checks and retains the non-zero primary scalar for a future reconnect attempt, without exposing it in trace/status. This is the only source-confirmed assignment of the AVCT reconnect value used by `b::jb()`. Do not substitute a value from an APCP setup tail or from AVMP. The optional second scalar remains profile-gated and unimplemented. |
| `0x8103` | Takes unsigned lengths at bytes `2` and `3`, copies the two byte regions beginning at byte `4` into the retained fields later supplied to the normal account-login encoder, then creates `com.avocent.c.f.i`. | The official parser has no short-record checks and directly handles account material. A replacement must bounds-check both regions, keep them in short-lived secret storage, and never log, trace, return, or persist them. This record remains unsupported until its exact launcher/state predicate is implemented. |

The corresponding client writers are also distinct and must not be emitted as
a generic post-attach sync:

| APCP type | Official class | Directly reachable serializer in the supplied iDRAC8 client |
|---:|---|---|
| `0x0102` | `com.avocent.c.f.f` | If its configuration selector is zero, `APCP | total=10 | type=0x0102`; otherwise the directly reachable branch is `APCP | total=16 | type=0x0102 | five zero bytes | zero byte`. The class contains an unselected 18-byte variant; it is not protocol permission to choose that variant. |
| `0x0103` | `com.avocent.c.f.e` | Fixed 48-byte record: `APCP | total=48 | type=0x0103 | u16(0) | u32(0) | 0x01 | 31 zero bytes`. |

`d.g.a.b` only queues these writers from its response-handler branches. The
source does not make either writer part of normal control login or video
attach, so neither is an allowed fallback for a missing frame.

### Complete modern AVCT receive-factory inventory

**S** `com.avocent.d.g.a.a#a(int)` is the official iDRAC8 AVCT receive
factory. The following is its complete explicit switch inventory, grouped by
purpose. It is intentionally an inventory, not an implementation backlog:
the presence of a type proves only that the official client recognises it. A
backend without the corresponding complete state path must surface a
structured unsupported-protocol fault instead of ignoring it.

| Incoming type(s) | Official receive class / static label where available | Repository boundary |
|---|---|---|
| `0x0080` | `mb`, Video Packet | display-control input; unsupported unless the owning display path proves its use |
| `0x0081/0x0082/0x0083/0x008a`, `0x8601/0x8602/0x8603/0x860a` | `kvm.a.a.j`, DVC envelope | the four decoder families recorded above |
| `0x0084` | `fc`, Video Connect Status | required console-start state input |
| `0x0085`, `0x8605` | `cb`, VideoStopped | official handler publishes `video_stopped`, resets display state, and acknowledges the record; the next DVC handler clears the stopped marker and decodes that same DVC record without requiring another `0x0084`; exposed as typed `no_signal`, never inferred from elapsed time |
| `0x0086`, `0x8606` | `kvm.a.c.g` | distinct stateful block-video path; its fixed envelope is recorded below, but the compressed command stream remains unsupported until ported and fixture-tested |
| `0x0087/0x8607` | `ec`, Font Table | recognised, unsupported |
| `0x0088/0x8608` | `ob`, Color Palette | recognised, unsupported |
| `0x0089/0x8609` | `sb` | recognised, unsupported |
| `0x008b/0x860b` | `cc` | recognised, unsupported |
| `0x8100` | `ub`, Keyboard LED | typed HID status, not a generic success reply |
| `0x8101` | `yb`, MouseAcknowledgeResponse | typed HID state input |
| `0x8102` | `xb`, MouseAccelResponse | typed HID state input |
| `0x8103` | `wb`, MouseAbsSyncResponse | typed HID state input; do not infer it from a pointer write |
| `0x8200`, `0x8206` | `tb`, Input Resolution Response | required display setup input |
| `0x8201` | `qb`, Display Resolution Response | required display setup input |
| `0x8202` | `gc`, VideoSetupData | required display setup input |
| `0x8203` | `dc`, Scaling | recognised display control; unsupported until its sender predicate is ported |
| `0x8204` | `q`, DVCColorModeResponse | DVC status only |
| `0x8205` | `pb`, DisplayResolutionInvalidResponse | terminal/recoverable display fault, never a frame |
| `0x8300`, `0x8305` | `c`, Login Response variants | required control-login input |
| `0x8301`, `0x8303`, `0x8304` | `hb`, `qc`, `w` | disconnect/preemption-related branch; explicitly unsupported until full policy is ported |
| `0x8302` | `ac`, Protocol Version | capability/control input; not a generic video-attach barrier |
| `0x8400`, `0x8401` | `nc`, SharingRequest variants | unsupported sharing branch |
| `0x8410` | `com.avocent.d.d.a.a` | profile-selected sharing/options list; parser contract below |
| `0x8411..0x8415` | `rb`, `vb`, `bc`, `d.d.a.b`, `nb` | exclusive/locked/reserved/session/options responses; unsupported branch |
| `0x8420` | `com.avocent.d.d.a.d` | recognised control response; parser contract below |
| `0x8430..0x8432`, `0x8434`, `0x8435` | `b`, `jc`, `kc`, `o`, `n` | chat/credential/server-status family; unsupported branch |
| `0x9e10`, `0x9e11`, `0x9f00` | `l`, `k`, `c` | recognised proprietary/custom-server family; unsupported |
| AVPT `0x8500`, `0x8510`, `0x8520`, `0x8530`, `0x8540` | `d.b.a`, `d.b.c`, `d.b.e`, `d.b.b`, `d.b.d` | a separate `AVPT`-magic auxiliary channel, not `BEEF` or AVMP; exact raw parsers below and product support is disabled |
| any other type | `zb`, NoOpResponse fallback | terminal unsupported-protocol fault, with only type and length in diagnostics |

The factory additionally routes `0x0400` to the AVCT `KeepAlive` class and
uses the zero-magic video-channel-auth record separately. Those are client
writers and must not be confused with BMC-originating messages solely because
the decoder has a nominal class for their type. This direction distinction is
why a native backend must own an explicit state machine rather than dispatching
every numeric type through one generic handler.

### AVPT auxiliary records (`0x8500`..`0x8540`)

The modern reader has a third framing branch in addition to `BEEF` and
`APCP`: a header whose magic is exactly `AVPT`. **S** it reads
`total_len:u32be` at header bytes `4..7`, then dispatches the u16 type at
payload offset zero through `com.avocent.d.g.a.a#b(byte[], int)`. These values
must not be merged with AVMP server-image records of the same numeric range:
they have a different magic, receive factory, and downstream handler.

| AVPT type | Official class | Exact parser boundary, relative to the AVPT payload |
|---:|---|---|
| `0x8500` | `com.avocent.d.b.a` | `raw_u16@2, entry_count:u16@4`, then exactly `entry_count` four-byte entries `raw_byte, raw_byte, raw_u16be` from offset 6. |
| `0x8510` | `com.avocent.d.b.c` | `raw_u16@2, raw_u16@4`. |
| `0x8520` | `com.avocent.d.b.e` | `raw_u16@2, length:u16@4, bytes:[length]@6`. |
| `0x8530` | `com.avocent.d.b.b` | receives the record but parses no payload bytes. |
| `0x8540` | `com.avocent.d.b.d` | `raw_u32be@4`. |

All five classes have no outbound serializer. `d.g.a.b` only forwards their
common base object to an optional local consumer when its separate local
predicate is true; it assigns no source-visible browser/console meaning.
Therefore a backend must validate the declared AVPT total and every listed
length, then return an immediate unsupported-auxiliary-protocol fault until a
complete official producer/consumer state path is ported. It must not leave
the video source waiting for a frame.

### AVCT sharing, options, and proprietary receive families

The following completes the byte-level receive boundary for the modern
factory's non-video control families.  It does not make sharing, chat,
credential, or proprietary server features product-supported.  All layouts
are **S** from the concrete parsers named in the factory; field labels are
limited to code-visible behaviour.

| Type | Parser | Bounded raw parser contract |
|---:|---|---|
| `0x8301` | `hb` | first payload byte is retained as a signed raw value |
| `0x8303` | `qc` | branch-dependent fixed record described below |
| `0x8304` | `w` | byte 0 plus the two following bytes combined by the official code; consume at least 3 bytes |
| `0x8400/0x8401` | `nc` | payload `u16@0`, byte `@2`; only the `0x8401` constructor retains non-zero bytes from offset 3 to the declared packet end as a string |
| `0x8410` | `d.d.a.a` | profile-selected compact name list or typed TLV stream described below |
| `0x8411` | `rb` | payload byte 0, whose only exposed predicate is `> 0` |
| `0x8412/0x8413` | `vb` / `bc` | recognised, parser consumes no payload bytes |
| `0x8414` | `d.d.a.b` | payload byte 0; exposed predicate is exactly `byte == 1` |
| `0x8415` | `nb` | nested length-prefixed records described below |
| `0x8420` | `d.d.a.d` | count byte followed by exactly `count` triples; tags 1, 3, and 5 set three separate booleans when their triple's value byte is 1 |
| `0x8430` | `b` | u16 entry count, followed by variable entries described below |
| `0x8431` | `jc` | `string_len:u16be | string:[len]` |
| `0x8432` | `kc` | packed u16, first string length/u16 + bytes, second string length/u16 + bytes |
| `0x8434` | `o` | two fixed 32-byte, NUL-terminated byte regions |
| `0x8435` | `n` | final four bytes are a u32; preceding bytes are NUL-trimmed text |
| `0x9e10` | `l` | selected parser consumes no response bytes; its constructor's unrelated `0x0306` writer must not be treated as a `0x9e10` response contract |
| `0x9e11` | `k` | APCP-magic writer class present in the AVCT factory; its receive method consumes no response bytes |
| `0x9f00` | `c` | retains the eight-byte header and whole payload; if header magic is `CSCO`, exposes u16s at header offsets 4 and 6 |

`qc(0x8303)` first reads two bytes.  A client-global configuration predicate
then selects either a third one-byte scalar or a four-byte big-endian scalar.
It next reads one length byte and a fixed 96-byte region, clamping only the
string extraction length to 96.  In the four-byte-scalar branch it additionally
reads a second length byte, a fixed 40-byte region, and one trailing byte.
The source does not name the configuration predicate or these values.  A
replacement must pick its branch from an explicit compatibility profile,
validate the entire selected fixed length before reading, and preserve raw
values.  It must not use the current process configuration as an implicit
wire-protocol discriminator.

For `nc(0x8400/0x8401)`, the official code derives the string length from the
outer AVCT total length (`total - 8 - 3`), not from a payload field.  Its
signed-byte shifts are not a reliable parser specification.  A safe decoder
must require `payload_len >= 3`, read the first two bytes as an unsigned
big-endian scalar, retain byte 2 raw, and (only for the profile whose parser
uses it) read exactly the remaining bounded bytes without assuming an
encoding beyond the source's byte-to-char conversion.

`d.d.a.a(0x8410)` chooses a format from the session's negotiated protocol
pair.  For pairs lower than `(2,34)`, payload byte 0 is a count and each
element is `name_len:u8 | name:[name_len]`.  For `(>=2,34)`, it is a sequence
of `tag:u8 | length:u8 | value:[length]` entries terminated by tag zero.
Tags `1`, `2`, and `6` are interpreted by the official code as big-endian
u32; tag `3` and `5` as a string of `length - 1` bytes; and tag `4` as a
string of `length` bytes.  The caller's list-entry boundary is tag zero.
Unknown tags are skipped by their declared length.  The Java parser lacks
all short-record checks; a replacement must reject a zero length where the
known tag requires four bytes, a missing terminator, or any value extending
past the AVCT payload.  The static property keys assigned by this parser are
intentionally not promoted to public semantics here.

`nb(0x8415)` starts at payload offset 2, takes a record count from byte 2,
and then consumes that many records beginning `record_len:u8 | kind:u8`.
For kind `2`, it treats the entire record as a display-list blob: byte 0 must
be at least 2 and `(byte0 - 2)` is divided into 4-byte pairs of u16 values
starting at offset 2.  Other conditionals in the decompiled class have an
impossible comparison and do not yield a reliable additional layout.  The
only safe current policy is to validate each record length and reject any
unrecognised kind with an unsupported-control fault.

For `b(0x8430)`, the first u16 is the entry count.  At each entry the official
code reads a packed u16, then `first_len:u16 | first:[first_len]`, then
`second_len:u16 | second:[second_len]`.  `kc(0x8432)` is the same one-entry
shape.  The parser masks the first packed field with `0x0003` and derives a
second raw value using `(packed & 0x0032) >> 5`; that exact non-contiguous
mask must be retained rather than renamed as a bitfield capability.  Both
classes require strict bounds checks for every declared length.

`o(0x8434)` and `n(0x8435)` demonstrate why unsupported messages cannot be
silently ignored: the former unconditionally reads 64 bytes, while the latter
expects an outer total of at least 12 and consumes a trailing status-like
u32.  On a short payload either must become an immediate malformed-control
fault.  The official factory's permissive construction is not a licence to
let a browser-side session remain pending.

### `0x0086` / `0x8606`: distinct block-video envelope

The receive-factory row above is not a generic DVC alias. **S**
`com.avocent.kvm.a.c.g#a(byte[],byte[],int)` parses its payload as follows;
all offsets are relative to the AVCT payload and the type may be either
`0x0086` or `0x8606`:

```text
0       raw selector byte
1..3    packed 24-bit value; the official client exposes high/low 12-bit parts
4..5    first u16 dimension
6..7    second u16 dimension
8       raw flags; bits 0,1,2,3,4 are retained separately
9..10   packed u16; the official client exposes fields 6/6/4 bits
11      raw byte; the official client exposes low/high four-bit parts
12..    compressed block-video data
```

The parser copies the compressed suffix exactly and supports continuation by
appending a subsequent `g` message. `com.avocent.d.g.a.b#a(g)` retains a
prior packet when flag bit 0 is set and otherwise appends/forwards the new
packet, then acknowledges the video packet using the ordinary AVCT
acknowledgement path. The downstream worker is
`com.avocent.kvm.a.c.e`/`b`: it is a stateful block decoder with quantisation,
bit-reader, and transform tables, not the DVC RLE reader
(`com.avocent.kvm.a.a.c`).

The dispatcher-to-decoder hand-off is also statically traced. It passes the
two u16 values at payload `6` and `4` as the worker's first two dimensions;
the raw selector byte; the high then low nibbles of byte `11`; the copied
suffix; the low four bits of packed bytes `9..10`; both 12-bit halves of the
packed 24-bit value; both six-bit fields of packed bytes `9..10`; and flag
bit 4. These are the exact argument positions of
`kvm.a.c.e#a(...)`, not inferred display terminology. In the worker:

- raw selector `2` or `3` stores an auxiliary object keyed by the low
  four-bit packed field and does not enter the block-command loop;
- every other selector first applies the low bit of the selector and the two
  nibbles from byte `11` to the decoder configuration;
- it consumes the compressed suffix as `suffix_length / 4` little-endian
  32-bit words, retaining word-reader state across packets;
- a non-zero low four-bit packed field selects an additional stateful
  framebuffer/configuration path using the two 12-bit and two six-bit values.

This proves that neither byte 0 nor the packed fields may be collapsed into a
DVC selector, colour depth, or frame rectangle. The source-visible state
machine is necessary but not sufficient to implement the JPEG-like command
stream: every command must still be exercised against the official decoder
before this path can publish frames.

#### Block-video command reader and opcode closure

**S/O** The worker consumes the copied suffix as little-endian `u32` words,
but reads bits most-significant-bit first from each loaded word. This is not
network-order bit packing: `b#a(byte[], 0, word_count)` forms each word as
`b0 | b1<<8 | b2<<16 | b3<<24`, while `b#b(bits)` extracts from bit position
`32 - bits`. A replacement has to reproduce both facts, including state
across word boundaries, before applying any entropy decoder.

The parser's block edge is `8` when `(raw_selector & 1) == 0`, otherwise
`16`. It configures the block worker with selector-low-bit and the high/low
nibbles of payload byte 11 before it reads command words. The high four bits
of the current bit reader word select the following complete opcode set in
`kvm.a.c.e#b()`:

| High nibble | Official bit consumption and action | Parser outcome |
|---:|---|---|
| `0` | consume 4 bits, then call the three-/six-plane entropy, inverse-transform, and colour-conversion path with transform variant `0` at the current block cursor | advances one block |
| `4` | consume 4 bits, same entropy/transform path with variant `2` | advances one block |
| `5` | consume 4 bits; configure one component from 2-bit index, 1-bit presence, optional 3 bits, and a 27-bit value; call the component-colour path with variant `0` | advances one block |
| `6` | as `5`, repeated for two components | advances one block |
| `7` | as `5`, repeated for four components | advances one block |
| `8` | read cursor `x` and `y` from packed bits `27..20` and `19..12`, consume 20 bits, then use transform variant `0` | explicit-cursor block |
| `9` | sets the decoder stop flag without consuming a block payload | stop marker; it does not mean DVC end-of-frame |
| `12` | as `8`, with transform variant `2` | explicit-cursor block |
| `13` | as `8`, then configure one component as in `5`, and use component-colour path variant `0` | explicit-cursor block |
| `14` | as `8`, then configure two components; their 27-bit values are masked to `0x00ffffff`; use component-colour path variant `0` | explicit-cursor block |
| `15` | as `8`, then configure four components as in `5`; use component-colour path variant `0` | explicit-cursor block |
| `1`, `2`, `3`, `10`, `11` | no matching branch | official error/stop path (`-1`) |

For the component records, a presence bit of zero consumes the following
three bits before the 27-bit value; a presence bit of one does not. The
official code then stores the selected value in a four-entry component table.
No presentation-level name is assigned to that table here.

The transform path decodes one DC and up to 63 AC coefficients through the
official fixed Huffman tables, zig-zags coefficients into a 64-element block,
applies the four static quantisation/transform table sets, and writes opaque
ARGB pixels through its three-/six-plane conversion tables. These are static
arrays in `kvm.a.c.b`; they are codec constants, not BMC-negotiated fields.
The component-colour path instead expands the configured component table over
the current block and uses the same final conversion function. Both paths
write into the worker's scratch framebuffer; only after a positive block
output does `kvm.a.c.e` copy the changed block rectangle to the display
framebuffer.

The following **O** (official in-memory decoder) probes use an `8x8` worker,
raw selector/nibbles/packed fields all zero, one synthetic little-endian word
whose high nibble is the opcode, and zero words thereafter. The hash is
SHA-256 over 64 resulting ARGB pixels in big-endian order. They do not model
BMC traffic; they guard the static command dispatch against a port changing
word order, valid-opcode set, or error semantics.

| Opcode | Official return | ARGB digest |
|---:|---:|---|
| `0` | `64` | `2d40473fc07daf75d666e1896b417013284f701e6f4b2d0e852dd438d1d80847` |
| `1` | `-1` | `a9faedf005419034af9a0adfb822237e183bb5e6315f586c2abfa999e13125fa` |
| `2` | `-1` | `a9faedf005419034af9a0adfb822237e183bb5e6315f586c2abfa999e13125fa` |
| `3` | `-1` | `a9faedf005419034af9a0adfb822237e183bb5e6315f586c2abfa999e13125fa` |
| `4` | `64` | `2d40473fc07daf75d666e1896b417013284f701e6f4b2d0e852dd438d1d80847` |
| `5` | `64` | `a9faedf005419034af9a0adfb822237e183bb5e6315f586c2abfa999e13125fa` |
| `6` | `64` | `a9faedf005419034af9a0adfb822237e183bb5e6315f586c2abfa999e13125fa` |
| `7` | `64` | `a9faedf005419034af9a0adfb822237e183bb5e6315f586c2abfa999e13125fa` |
| `8` | `64` | `2d40473fc07daf75d666e1896b417013284f701e6f4b2d0e852dd438d1d80847` |
| `9` | `-4` | `a9faedf005419034af9a0adfb822237e183bb5e6315f586c2abfa999e13125fa` |
| `10` | `-1` | `a9faedf005419034af9a0adfb822237e183bb5e6315f586c2abfa999e13125fa` |
| `11` | `-1` | `a9faedf005419034af9a0adfb822237e183bb5e6315f586c2abfa999e13125fa` |
| `12` | `64` | `2d40473fc07daf75d666e1896b417013284f701e6f4b2d0e852dd438d1d80847` |
| `13` | `64` | `a9faedf005419034af9a0adfb822237e183bb5e6315f586c2abfa999e13125fa` |
| `14` | `64` | `a9faedf005419034af9a0adfb822237e183bb5e6315f586c2abfa999e13125fa` |
| `15` | `64` | `a9faedf005419034af9a0adfb822237e183bb5e6315f586c2abfa999e13125fa` |

`DumpBlockVideoReference.java` is a local, ignored oracle harness for these
results. It instantiates only the supplied JAR's decoder and in-memory
framebuffer; it never opens a socket or reads a BMC record. A Rust port still
requires fixtures for every valid opcode, both block edges, continuation,
component-presence branch, and malformed/truncated words before runtime
support can be enabled. Until then `0x0086/0x8606` remains a structured
unsupported-video-source fault rather than a source of synthetic frames.

For reproducibility, the opcode-0 probe above requires a padded reader input:
the first word is `00000000` and the harness supplies 63 following zero
little-endian words. Running it with only one or two words makes the official
decoder read beyond its synthetic input and is not a protocol result. With 64
words, the official harness returns `64` and the recorded
`2d40473f…d1d80847` digest.

The field names above deliberately describe only the operations exposed by
the code. The source does not attach a product label to the selector, flags,
or packed fields. Until the `kvm.a.c.b` bit command stream has its own
official-output fixtures, this family must produce an immediate
`unsupported-protocol` video fault with the numeric type only; it must not be
routed through DVC, black-filled, or left in a first-frame wait.

### Auxiliary display packet parsers (`0x0087`..`0x008b`)

These types are recognised by the same display dispatcher but do not share
the DVC envelope. The following fixed parsers are **S** from the corresponding
official receive classes. They provide a safe parsing boundary for a future
renderer; they do not establish that the fields have browser-facing names.

| Type(s) | Class | Exact parser boundary | Current policy |
|---:|---|---|---|
| `0x0087`, `0x8607` | `ec` | Payload starts `u16@4, u16@6, flags@8, byte@9, byte@10, byte@11`. Flags 0..4 are retained. A 12-byte packet is a no-suffix variant; a 16-byte packet with bits 0 and 1 set has `u16@12, byte@14, byte@15`; otherwise the class copies the suffix beginning at 12 and, when bit 1 is set, reads its final four bytes as `u16, byte, byte`. | A stateful renderer exists in the official client but is not yet ported; validate each branch length then fault. |
| `0x0088`, `0x8608` | `ob` | `count:u16be@0, raw_byte@2, raw_byte@3`, then exactly `count` RGB triplets from offset 4, converted by the official client to opaque ARGB. | Validate `4 + 3*count <= payload_len`; do not substitute it for the DVC7 fixed palette. |
| `0x0089`, `0x8609` | `sb` | `raw_byte@0, raw_byte@1, u16be@2, suffix@4..`. The class retains all suffix bytes. | Renderer/state dependency unported; fault after bounded parse. |
| `0x008b`, `0x860b` | `cc` | Variant byte at 0 selects fixed prefix size: variant 0 -> 6; 1 -> 16; 2/3 -> 12; 4/5/6 -> 12; 7 -> 4; 8 -> 8. The remaining bytes are retained as suffix. Variants contain only the raw bytes/u16s read at those prefix offsets by `cc#a`; no common geometry is assumed. | Reject unknown variants or short prefixes; all known variants remain unsupported until their specific official renderer path is ported. |

The iDRAC8 dispatcher feeds these packets into separate renderer adapters and
only then calls the shared acknowledgement counter. Consequently a native
backend may not acknowledge and publish a synthetic frame after merely
recognising one of these records.

### Source-confirmed outbound controls outside the current feature surface

Completeness of a receive factory does not by itself inventory all records the
official client can emit. The following additional AVCT writers are reachable
from the iDRAC8 viewer source and are intentionally catalogued here so that a
backend cannot accidentally repurpose their bytes as a console bootstrap.

| Type | Official class | Byte-level contract | Source trigger / repository policy |
|---:|---|---|---|
| `0x0310` | `com.avocent.c.d.fb` | total 36; 28-byte payload. Bytes `0..1` are a caller-supplied two-byte tag; `2..3` and `4..5` are caller-supplied u16s; `6..17`, `24`, and `27` are zero; `18..19`, `20..21`, `22`, `23`, `25`, `26` are independent setters. | The `tb` input-resolution handler creates it only when its four-byte format tag is literal `P3`; unsupported until the full sender predicate and all field meanings are ported. |
| `0x020a` | `com.avocent.c.d.ab` / base `z` | total 16; payload byte 0 is caller-supplied and bytes `1..7` zero. | The source class does not establish a browser-facing operation name; do not emit. |
| `0x0320` | `com.avocent.c.d.db` | total `12 + string_length`; payload `u16 value, u16 string_length, raw string bytes`. | A session-side event/property branch; unsupported. |
| `0x0410` | `com.avocent.c.d.pc` | total 16; payload byte 0 is caller-supplied and bytes `1..7` zero. | Sharing branch; unsupported. |
| `0x0411` | `com.avocent.c.d.gb` | total 16; eight zero payload bytes. | Sharing branch; unsupported. |
| `0x0412` | `com.avocent.c.d.mc` | total 16; eight zero payload bytes. | Legacy preemption branch; unsupported. |
| `0x0413` | `com.avocent.c.d.oc` | total 16; byte 0 is a caller-supplied low byte, byte 1 zero, bytes `2..3` a caller-supplied u16, remaining bytes zero. | Sharing branch; unsupported. |
| `0x0414` | `com.avocent.c.d.d` | total 16; payload byte 0 is the constructor boolean as `0/1`, bytes `1..7` are zero. | Reached only through a separate session operation; its product meaning is not recovered, so do not emit. |
| `0x0416` | `com.avocent.c.d.lc` | total 16; payload bytes 0 and 1 are two independent caller-supplied low bytes, remaining bytes zero. | Preemption branch; unsupported. |
| `0x0427` | `com.avocent.c.d.y` | total 16; payload is `u32be(30)` then four zero bytes. | Credential-request branch; unsupported. |
| `0x0428` | `com.avocent.c.d.m` | total 264; payload is a fixed 256-byte string region. | Its operation label/predicate was not recovered from the launcher path; unsupported. |
| `MGMT` / `0x9e90` | `com.avocent.c.f.b` | Four-byte magic `MGMT`, then `type:u16be`, `total:u16be`; payload is five raw/boolean bytes, sixteen four-byte records, then a source-counted sequence of `raw_byte, raw_byte, bool, raw_byte, raw:[80]` records. | Reached only through session operation 9 with a `c.f.m` object; this is a distinct framing family and is unsupported. |

This table names no field whose meaning is not assigned by the official
class. It establishes only the exact serialiser shape and the rule that none
of these records is an allowable generic retry, attach, or teardown message.

Several message classes use internal queue identifiers that are not their wire
types. In particular, modern `rc`/`sc` and legacy `ec`/`fc` are constructed
with `0x003f` but serialize `0x0201`/`0x0209`; `c.f.c`, `c.f.i`, `c.f.j`,
`c.f.g`, and `c.f.h` similarly carry local dispatch ids
`0x009e`, `0x1fa7`, `0x1fa6`, `0x2009`, and `0x5555` respectively, while
their actual wire records are documented in the APCP sections. An
implementation must derive its outgoing type from the serializer header,
never from that internal constructor argument.

### HID wire contracts

The current keyboard encoder matches the official `f` constructor exactly:

```text
BEEF 0200 0010 | 00 | release:u8 | usage:u16be | 00 00 00 00
```

The constructor's boolean is true for a press, while wire byte 1 is zero for a
press and one for a release. HID usages are passed as `u16`; they are not
translated to PC scan codes by this AVCT layer.

The iDRAC6 reference viewer follows the same per-edge contract. Its
`com.avocent.kvm.b.f#a(int)` and `#b(int)` methods each construct one
`com.avocent.kvm.b.a.f(usage, pressed)` and enqueue it in
`com.avocent.kvm.c.d.g`. That queue wakes its writer with `notifyAll`; the
writer serializes header and payload and calls `OutputStream.flush()` for every
packet. There is no extra keyboard commit record and no press/release batch.

The absolute mouse contract is `rc` (`0x0201`):

```text
BEEF 0201 0010 | 00 | state:u8 | x:u16be | y:u16be | value:u16be
```

Its companion `sc` (`0x0209`) has the same payload fields with a different
message type and an additive base-class flag. The official input façade,
mouse-mode selection, and browser-mask adaptation are recorded in the next
section.

`0x0202` (origin), `0x0203` (reset), `0x0204` (LED request), `0x0208` (focus),
and `0x020b` (absolute-sync) have confirmed eight-byte encoders. `0x020b`
contains two encoded booleans rather than an all-zero tail; its exact layout
is recorded below. They are separate operations; emitting an ordinary
`0x0201` packet is not a protocol equivalent for reset/focus/sync.

### HID façade, mode, and button-state contract

The packet constructors alone do not establish which packet a user action
must select. That selection is now also statically traced through the official
iDRAC8 viewer's public input façade and its AWT input handlers. This is the
boundary used by a browser-facing implementation; it is not derived from a
packet capture.

**S** `com.avocent.a.b.w#a(int,int,int,int)` emits `rc` (`0x0201`), while
`w#b(int,int,int,int)` emits `sc` (`0x0209`). Both carry the same eight-byte
payload shape:

```text
0       zero
1       accumulated pointer-state value, low byte
2..3    first coordinate, u16be
4..5    second coordinate, u16be
6..7    caller-supplied signed value, i16be (wheel delta in the AWT path)
```

The `rc` encoder clamps a negative coordinate to zero; `sc` does not. The
official higher-level handlers select `rc` for absolute pointer events and
`sc` for relative/delta events. They do not use a relative coordinate with
the absolute packet simply because its wire layout is similar. The façade's
additional constructor value (`1/2/4/8`) is message-queue metadata passed to
the superclass; it is not serialised in the eight-byte AVCT payload. Its
exact scheduling semantics are not required to parse or encode the packet and
remain unnamed.

**S** `com.avocent.app.c.g.e` and `com.avocent.app.c.g.f` maintain the
official pointer-state field on press/release exactly as follows:

| Physical button as received by the Java UI | AVCT pointer-state bit |
|---|---:|
| left (AWT button 1) | `0x0001` |
| middle (AWT button 2) | `0x0004` |
| right (AWT button 3) | `0x0002` |

This agrees with the repository's own browser event contract, independently
of web-platform documentation: `frontend/src/features/hid/useHidInput.ts`
serialises left/middle/right as button indices `0/1/2`; `HidEventCodec` turns
those into feature masks `1/2/4`; and the AVCT adapter must translate those
feature masks to the official AVCT values `1/4/2`. A three-button press is
therefore AVCT `0x0007`. Buttons outside that set have no official
three-button façade mapping and must be rejected, not silently aliased.

The repository's generic HID event format represents one button index per
press/release. The feature contract now carries that transition explicitly and
the AVCT adapter owns the accumulated mask, so releasing one button preserves
the other held buttons. This is covered by an offline transition oracle;
buttons outside the three source-confirmed mappings are rejected.

The iDRAC6 wheel path confirms that wheel and button state are independent
fields. `com.avocent.kvm.c.cb#mouseWheelMoved` negates
`MouseWheelEvent.getWheelRotation()` and calls
`lb#a(x, y, wheel, false, false, false, clickCount)`. Its concrete
implementation `com.avocent.kvm.b.g#a` retains the current accumulated button
mask, then constructs `com.avocent.kvm.b.a.ec(x, y, wheel, mask, 4)`.
The constructor assigns its third argument to `f` and fourth argument to `g`;
`ec#b()` serialises `g` (the mask) at payload byte 1 and `f` (the signed wheel
value) at payload bytes 6..7. A wheel-only packet must therefore carry
`mask=0`; middle button remains the distinct `0x0004` mask above.

The remaining dedicated packet constructors are byte-confirmed by the modern
client:

| Type | Class | Payload | Official call path established here |
|---:|---|---|---|
| `0x0202` | `h` | eight zero bytes | façade operation 4 / session dispatcher |
| `0x0203` | `i` | eight zero bytes | constructor confirmed; caller predicate not required by the browser façade |
| `0x0204` | `g` | eight zero bytes | session-side keyboard LED request |
| `0x0208` | `e` | `enabled:u8, 0:[7]` | façade boolean operation; it also updates local focus state |
| `0x020b` | `v` | `first_bool:u8, second_bool:u8, 0:[6]` | session dispatcher operation 10; constructor defaults to `[1,0]` and the public setter changes only byte 1 |

The latter two booleans are source-confirmed encodings, but their product
labels beyond the official operation names are not established. In particular,
`0x020b` is not evidence that a plain pointer movement has synchronised the
remote pointer. Reset, origin, focus, LED, and sync must stay distinct typed
operations in a native backend.

### iDRAC8 native VM engine: protocol boundary and canonical encoders

The iDRAC8 VM launcher delegates transport, login, virtual-disk, and
reconnect work to the official `libVMAPI_DLL.so`. Its Java classes are JNI
facades, not an alternate wire implementation. The iDRAC6 Java VM classes
are corroboration only; they are not authority for the iDRAC8 native VM
channel where the native library differs.

**N** All symbols and offsets in this section are from
`libVMAPI_DLL.so`, identified in the artifact table above. This is static
binary inspection only: no device traffic, credentials, or network capture is
used as evidence.

#### AVMP base framing and record ownership

`AVMPPacket::encodeHeader(short,int,unsigned char*)` (`0x000da08a`) writes
this 12-byte header. Its `decodeHeader` counterpart reads only the total length
and type; subclasses own the final two bytes.

```text
0..3   "AVMP"
4..7   total packet length, u32be, including all 12 header bytes
8..9   packet type, u16be
10..11 subclass header extension
12..   payload
```

`GetVDiskInfo` is the sole base special case: the shared header writer puts
`0xffff` at bytes `10..11` for type `0x0200`. `VDiskRequest`,
`VDiskRelease`, `ReadRequest`, `ReadResponse`, `DeviceStatus`, and
`ClientStatusMessage` explicitly write their remote-disk id there. A receiver
must therefore retain the extension as raw header state; it is not a generic
session id.

This `0xffff` discovery sentinel is **N** for the iDRAC8 native VM engine,
not a generation-independent AVMP axiom. The supplied iDRAC6 Java
`PacketAVMP#setMessageType(0x0200, ...)` writes `0x0000` at the same header
extension and its discovery call site does not overwrite it. Both clients
agree on the 12-byte geometry and the later remote-disk extension ownership,
but a shared iDRAC6/8 backend must select the discovery extension from its
generation-specific protocol profile rather than hard-code either value.

The two APCP request classes first invoke that base header writer and then
overwrite bytes `0..3` with `"APCP"`. Setup packets therefore use the same
u32/u16/u16 header geometry, but are APCP records and must not be fed to the
post-TLS AVMP packet receiver.

#### Native APCP request state

`ProtocolAPCP::ProtocolAPCP` (`0x000c85b0`) defaults to AVMP version `1.14`,
a 32-byte random, base capability vector `0x0000000d` (or `0x0000000c` in its
alternate constructor branch), and reconnect-check value `20`. The initial
writer (`ProtocolAPCP::sendSessionRequest`, `0x000c8e20`) ORs `0x00000600`
into that vector unless its pre-existing value is exactly `8`; it ORs
`0x00000800` when the corresponding library configuration boolean is set.

Those bit names are not recovered from the binary. The only proven facts are
their selection conditions and that the native default path serialises
`0x0000060d`; names such as “TLS capability” must not be attached to the
individual bits in product code.

The initial `SessionRequest` (`0x0100`) is exactly 68 bytes:

```text
0..3    "APCP"
4..7    total = 68
8..9    type = 0x0100
10..11  APCP profile = 0x0103
12      protocol type supplied to ProtocolAPCP (VM caller uses 2)
13      AVMP major
14      AVMP minor
15      zero
16..19  capability vector, u32be
20      random length
21..52  random, zero-padded to 32 bytes
53..55  zero padding in this writer
56..57  reconnect timeout, u16be (180 by default; 30 when the official
        `setClearTextReconnect(true)` flag is active)
58..59  keepalive timeout, u16be (native initial path: 0)
60..63  session id, u32be (native initial path: 0)
64..67  zero padding in this writer
```

The reconnect writer is not the same message with a changed tail.
`ProtocolAPCP::sendSessionRequestReconnect` (`0x000c9110`) emits
`SessionRequestEx`, type `0x0101`, total 68, and profile `0x0000`. Its common
prefix is identical only through byte 52. Its tail is:

```text
53..57  zero padding in the specialised encoder
58..59  timeout = 20, u16be
60..63  session id, u32be
64..67  zero padding in the specialised encoder
```

The official reconnect path alone selects `SessionRequestEx`; an initial
connection must not choose it merely because it has a session-id-looking
value.

The four final zero bytes in both records are not an inference from their
declared length. `SessionRequest::encodePayload` and
`SessionRequestEx::encodePayload` first zero all 56 payload bytes; their
last explicit write ends at payload byte 51. This is also why a generic
`68-byte APCP tail` struct is incorrect: the initial encoder writes its
first non-zero tail field at outer byte 56, while the reconnect encoder first
writes at outer byte 58.

#### Native APCP response: fields, branch discriminators, and limits

`ProtocolAPCP::receiveSessionSetup` (`0x000c973a`) reads ten bytes first and
then parses this shared prefix:

```text
0..3    "APCP"
4..7    total length, u32be
8..9    marker: 0x8100 or 0x8101
10..11  negotiated APCP profile, u16be
12      AVMP major
13      AVMP minor
14..17  capability vector, u32be
18..19  redirect TCP port, u16be
20      random length
21..52  random value/padding when random length is 32
```

The native engine retains the negotiated profile, version, capability vector,
redirect port, and random buffer. It accepts marker `0x8100`; marker `0x8101`
activates a distinct response branch. For the native VM engine, transport
choice is made in this order from the response capability vector: bit `0x1`,
then bit `0x8`, then bit `0x4`. This ordering is VM-native evidence only; it
must not be silently substituted for the AVCT-console Java transport chooser.

The bytes after the random padding are branch-specific. The parser proves the
following offsets relative to byte 10 (the byte after the first ten-byte
header):

| Marker/layout branch | Consumed tail offsets | Proven handling |
|---|---|---|
| `0x8100`, total 68 | `44:u16`, `46:u16`, `49:u8` | stores the first two values in separate session fields and uses the final byte as a branch value |
| `0x8101` | `44:u16`, `49:u8` | stores the u16 in the first of those fields and uses the same final-byte branch value |

The official symbols do not give product names to these tail values. They are
therefore recorded as raw fields. In particular, treating response bytes
`18..19` as a reconnect timeout or treating an arbitrary four-byte tail slice
as a connection id is not justified.

#### Native password-login encoders

`AVMPSession::sendLoginUser` (`0x000bf3ba`) selects `LoginMessage` only for
the normal user/password credential mode. It constructs it with
`avmp_major * 100 + avmp_minor`. `LoginMessage::LoginMessage(int)`
(`0x000c6de0`) changes the message type from `0x0100` to `0x0104` only when
that value is greater than `113`.

The normal fixed form has **total** length 153, not a 153-byte payload. Its
payload is exactly 141 bytes (`LoginMessage::encodePayload`, legacy branch at
`0x000c773d`):

```text
payload 0        username length, u8 (official encoder rejects >95)
payload 1..96    zero-padded 96-byte username field
payload 97..128  zero-padded 32-byte password field
payload 129..136 8-byte RIP/token field
payload 137      appliance port byte
payload 138      appliance channel byte
payload 139      zero
payload 140      preempt boolean (0 or 1)
```

The versioned form (`0x0104`) has total length `64 + username_len +
password_len`; its payload is `52 + username_len + password_len`:

```text
payload 0..1     username length, u16be
payload 2..3     total_packet_length - username_len - password_len, u16be
payload 4..5     password length, u16be
payload 6..37    zero bytes
payload 38..45   8-byte RIP/token field
payload 46       appliance port byte
payload 47       appliance channel byte
payload 48       zero
payload 49       preempt boolean (0 or 1)
payload 50..     username followed immediately by password
final 2 bytes    zero padding after the password
```

The versioned payload is therefore `52 + username_len + password_len` bytes:
the encoder's cursor reaches the end of the password at offset
`50 + username_len + password_len`, while its initial full-payload zero-fill
leaves the final two bytes untouched. This resolves the apparent `50` versus
`52` offset ambiguity: the credentials start at 50, but the payload's fixed
tail is two zero bytes. The literal `64` in the second field is a consequence
of the official total-packet calculation. It is not a new field semantic. The
native encoder bounds legacy usernames to 95 bytes and serialises a fixed
32-byte password field. A new implementation may impose stricter
allocation/input limits, but must not change the emitted layout.

`LoginResponse::decode` (`0x000dad2e`) parses a `0x8100` AVMP record as
`status:u8`, `flags:u8`, `username_len:u8`, then exactly that many username
bytes. This is a VM login response, distinct from the console's BEEF
`0x8300`/`0x8305` response families.

#### Native alternate credential modes

**N** `VMConnectionInfo` exposes four credential-type constants:
`0` default, `1` temporary credentials, `2` random number, and `3`
certificate. `AVMPSession::sendLoginUser` selects exactly three message
constructors from that field: type `2` selects `RandomLoginMessage`, type `3`
selects `LoginCertificateMessage`, and every other value selects the normal
`LoginMessage` path above. In all three branches it first marks the receive
worker as expecting a login response and queues the selected record through
the same `SendThread`.

`RandomLoginMessage` is an AVMP record (it does not override the base
`"AVMP"` header) with type `0x0102` and total length `22`:

```text
0..3    "AVMP"
4..7    total = 22
8..9    type = 0x0102
10..11  base extension = 0
12..15  client random, u32be
16..19  appliance random, u32be
20      raw mode flag (its only observed constructor value is 0)
21      preempt boolean (set by the named setPreempt method)
```

The native launcher supplies the two random values from the call arguments to
`sendLoginUser`; it does not derive them from the APCP random buffer. The
source gives no product name to byte 20 and no public setter changes it, so a
replacement must preserve it as a raw zero in this exact profile rather than
invent a sharing-mode meaning.

`LoginCertificateMessage` is a different AVMP record with type `0x0101` and
total length `12`; its payload encoder only zero-fills an empty payload. This
is a real wire collision with APCP `SessionRequestEx(0x0101)`: the record
magic, not the u16 type alone, distinguishes the two. The native constructor
contains no certificate material and the supplied artifact does not expose a
complete certificate-auth exchange after this marker. It is therefore a
verified selector/byte layout but remains an unsupported product branch.

The optional OTP/reconnect sequence is independently selected by
`AVMPSession::sendOTPRequest`, not by ordinary user/password login. Its
preconditions are: a send worker exists, the session's internal OTP-enabled
flag is non-zero, and its stored session-id slot equals `-1`. It emits two
APCP records in order:

```text
SessionIdRequest
  0..3   "APCP"
  4..7   total = 12 when constructor state <= 1, otherwise 16
  8..9   type = 0x0102
  10..11 base extension = 0
  12..15 present only in the total-16 form: 00 00 00 01

OTPRequest
  0..3   "APCP"
  4..7   total = 48
  8..9   type = 0x0103
  10..11 base extension = 0
  12..15 raw u32 field (the only observed sender leaves it zero)
  16      purpose byte (the only observed sender writes 2)
  17..47 zero
```

`SessionIdRequest` receives a first constructor integer, but its serializer
never reads that stored field. The only non-zero serialization decision is the
second constructor integer: values greater than one produce the 16-byte form
above. `sendOTPRequest` passes state `1` when one session configuration field
equals one, otherwise `2`; hence it selects one of the two byte forms without
serialising the first integer. This is a serializer fact, not a license to
assign a missing field semantic.

The receive factory constructs `SessionIdResponse` for `0x8102` and
`OTPResponse` for `0x8103`. Their native decoders establish:

```text
SessionIdResponse payload
  0..3  stored session id, u32be
  4..5  optional timeout, u16be, read only when total length > 16
          (decoder default is 60 when absent)

OTPResponse payload
  0     username length, u8
  1     password length, u8
  2..   username bytes followed immediately by password bytes
```

On a `SessionIdResponse`, the official processor stores the u32 value and, if
the optional timeout is positive, changes its reconnect-check interval to
`timeout / 2` (integer division). On an `OTPResponse`, it copies the two
strings into OTP-specific session slots only while the connection is in one
of the processor's accepted connection states. Neither parser bounds-checks
the declared lengths before allocation; a replacement must validate record
lengths before copying while preserving the established layout.

`PreemptResponse` is a 13-byte AVMP record, type `0x0120`, with base extension
zero and one payload byte. `AVMPSession::sendPreemptResponse` emits it only
when the session is not stopping and a send worker exists. It maps its API
argument `0x0134` to payload byte `0x34`; every other argument maps to zero.
The artifact does not provide a product-level label for either value, so a
native backend must not expose them as “accept” or “deny” until the caller UI
and peer handling are separately evidenced.

The backend has byte-oracle codecs for `SessionRequestEx`, `SessionIdRequest`,
`OTPRequest`, `SessionIdResponse`, and `OTPResponse`. The iDRAC8 runtime now
consumes a validated inbound `SessionIdResponse` without exposing its id,
applies its positive timeout as `timeout / 2`, and uses the documented native
reconnect writer branch after a transport loss from either the receive or
heartbeat-send path. It re-authenticates,
refreshes `VDiskInfo`, and remaps a retained readonly ISO before publishing a
recovered mapping. It does not emit `SessionIdRequest` or `OTPRequest`: the
supplied launcher does not expose the native OTP-enabled flag, so sending
those records would be a guessed trigger. iDRAC6 remains terminal on loss.
The discovery waiter also consumes a `SessionIdResponse` that arrives between
login and the first `VDiskInfo`; otherwise a valid early response would be
lost before the long-running receiver starts.

`Disconnect(0x8110)` is received as an AVMP record. Its decoder consumes:

```text
payload 0  reason:u8
payload 1  status:u8
payload 2  timeout:u8
payload 3  username_length:u8
payload 4  username bytes
```

The processor copies reason and timeout into `VMSessionStatus`, sets its
`can_preempt` flag only when raw status is `0x80`, invokes its session callback
with code `200` when registered, and otherwise treats the record as terminal
session state. The supplied strings name reason values `0,1,2,3,4,6` as
administrator, idle timeout, appliance reboot, DSRIQ upgrade, local-user
preemption, and KVM lock respectively. Those names are explicit iDRAC VM
evidence; all other numeric reasons and all status values except `0x80` remain
raw values.

#### Native readonly virtual-media data path

The following wire contracts are all **N** and apply to the native iDRAC8 VM
engine as well as agreeing with the iDRAC6 Java VM implementation where noted.

| Type | Direction | Exact layout / state evidence |
|---:|---|---|
| `0x0200` | client | `GetVDiskInfo`, total 12, extension `0xffff`, no payload. |
| `0x8200` | BMC | `VDiskInfo`: payload begins `count:u16`, then `type:u8,status:u8,capabilities:u16` per entry. The parser must validate `2 + 4 * count <= payload_len`. |
| `0x0210` | client | `VDiskRequest`: extension is remote disk id; payload is `block_size:u32, capacity:u32, drive_flags:u8, request_block_limit:u8, toc_len:u16, toc:[N]`. |
| `0x0211` | client | `VDiskRequest2`, a separate floppy geometry encoder; it must not be used for an ISO/CD image. |
| `0x8410` | BMC | `DeviceStatus`: extension is remote disk id; payload is one `u32` status. |
| `0x8300` | BMC | `ReadRequest`: extension is remote disk id; payload is `offset:u32, block_count:u32, max_blocks_per_packet:u32`. |
| `0x0300` | client | `ReadResponse`: extension is remote disk id; payload is `offset:u32, block_count:u32, data:[N]`; total length is recalculated to `20 + data_len`. |
| `0x0410` | client | `ClientStatusMessage`: total 16, extension is remote disk id, payload is one status `u32`. |
| `0x0220` | client | `VDiskRelease`: total 12, extension is remote disk id, no payload. |
| `0x0400` | client | `Heartbeat`: total 12, no payload; it is an AVMP record, not the APCP keepalive. |
| `0x01ff` | client | `SessionClose`: total 16; the exact native send-path result is recorded below because the object setter and transmitted buffer disagree. Its header magic is APCP. |

`RequestProcessor::process(ReadRequest*)` obtains the three request fields in
the order above. It chunks a request using the third field and sends one or
more `ReadResponse` records, followed by `ClientStatusMessage` status `0` on
success or a non-zero failure status. This proves that a single unbounded file
read or a response with the request's third field in the block-count position
is not compatible.

The supplied iDRAC6 VM constants define type `1` as floppy and type `2` as CD;
status `0` is idle, `1` attached, `2` disabled, and `3` broken. The iDRAC8
`VMRemoteDrive` wrapper independently defines type `2` as CD and type `4` as
floppy. Its native `VDiskInfo` parser preserves a type and status per entry
and reduces its u16 capability field to an enabled boolean. Selection must
therefore be generation-specific: a readonly implementation may use only the
idle CD or floppy type established for its negotiated profile, and must not
reinterpret an attached, disabled, unknown, or truncated entry as usable.

#### Native readonly virtual-media lifecycle

The following control flow is **N** from the named `AVMPSession` and
`RequestProcessor` symbols. It bounds the repository's readonly media feature;
it is not a claim that the separately decoded writable, smart-card, OTP, or
server-image branches are implemented.

```text
APCP SessionRequest -> negotiated socket -> LoginMessage
  -> LoginResponse(status == 0)
  -> GetVDiskInfo(0x0200, extension 0xffff)
  -> VDiskInfo(0x8200) -> choose an advertised remote-drive index
  -> VDiskRequest(0x0210) -> DeviceStatus(0x8410)
  -> zero or more ReadRequest(0x8300) / ReadResponse(0x0300)
       plus ClientStatus(0x0410) for every request outcome
  -> VDiskRelease(0x0220) -> SessionClose(APCP 0x01ff) -> socket close
```

`RequestProcessor::process(LoginResponse*)` sends discovery only on login
status zero (the direct VM-style branch), then starts the post-login workers.
The workers may include drive discovery, drive watching, heartbeat, and
reconnect depending on launcher configuration. Those configuration predicates
are not part of the readonly backend contract and remain unsupported unless
their complete branch is ported.

`AVMPSession::sendLoginUser` marks the receiver as expecting a login response
before it queues the request. When the configured heartbeat interval is
positive it starts `HeartBeatThread`, which sends AVMP `0x0400`; this is
independent of APCP's ten-second `0x0400` keepalive.

**N** `RequestProcessor::process(Heartbeat)` handles inbound AVMP `0x8400`
only while connected: it immediately calls `sendHeartbeatMsg()` to emit the
empty AVMP `0x0400` response, then checks the outstanding drive-info state.
The readonly worker implements this response; treating `0x8400` as an unknown
packet incorrectly terminates an otherwise healthy iDRAC8 media session.

**S** The supplied iDRAC6 Java `ApplianceSession` has a separate,
unconditional `ThreadDSSendHeartbeat`: while the session is running it polls
every `100 ms`; if `System.currentTimeMillis() - m_lLastPacketSentTime >
10000`, it queues a total-12 `PacketAVMP.setMessageType(1024, 12)` record
(`AVMP 0x0400`, no payload). Its output worker updates
`m_lLastPacketSentTime` only after a successful write and flush of *every*
outbound AVMP record. Therefore the iDRAC6 backend must use a fixed 10-second
output-idle heartbeat, rather than treating it as an optional configuration
or interleaving it with active read responses. The Rust state machine must
either implement both applicable keepalives with their observed start and
stop conditions or reject a negotiated path that requires them.

`RequestProcessor::process(DeviceStatus*)` treats payload status `0` as the
success branch. The iDRAC6 official constants corroborate the status table:
`0` OK, `1` command-not-supported, `2` read-failed, `3` write-failed,
`4` USB-not-connected, `5` drive-already-attached, `6` drive-disabled,
`7` unknown-configuration-option, and `8` virtual-disk-request-failed. Thus
waiting for `0x8410` status `1` as a mapping success is definitively wrong.
All non-zero statuses must become an immediate media feature fault carrying
the raw numeric status.

`AVMPSession::stopSession` stops receive/send-related workers, requests
`VDiskRelease` for mapped drives, queues `SessionClose` through `SendThread`
when the negotiated session is eligible and not reconnecting, waits for that
send queue to drain, and only then deletes the socket. Closing a TCP/TLS
stream without this ordered release is not protocol-equivalent. The exact
native `SessionClose` byte result is deliberately stated separately below;
the setter's name alone is not evidence that its stored id is on the wire.

#### Native reconnect scheduler, transport recovery, and completion boundary

The supplied iDRAC8 VM library has a complete reconnect state machine.  This
is **N** evidence from `ReconnectThread::{ReconnectThread,run,forceReconnect}`
at `0x000cc8e0`, `0x000ccdf8`, and `0x000cd258`,
`AVMPSession::reconnect` at `0x000bb5cc`, and the named request processors.
It is not interchangeable with the iDRAC6 Java VM client described at the
end of this subsection.

The native launcher's `VMConnectionInfo.reconnectMethod` is the source of the
APCP setup selector. It accepts raw values `0`, `1`, and `2`; both initial
`getSocket` and recovery `getSocketReconnect` pass selector `1` only for
method `1`, and selector `2` for methods `0` and `2`. This is a launcher
choice, not a value recovered from the APCP tail. `receiveSessionSetup` resets
its reconnect-capable state then sets it as follows: an `0x8101` response is
capable only when that stored selector is `1` or `2`; an `0x8100` response is
capable only when capability bits `0x0400` and `0x0200` are both set and the
negotiated APCP profile is strictly greater than `0x0101`. The backend now
preserves this exact result in its typed setup state.

`AVMPSession::StartLoginSuccessfulThreads` creates `ReconnectThread` when
either the stored APCP reconnect-available result is true or the session is
on the APCP-`1.02` branch.  It passes the reconnect-available result and the
session's reconnect-check timeout separately.  The thread constructor keeps a
positive supplied timeout; only when the availability argument is false and
that timeout is non-positive does it replace it with `30`.

`ASemaphore::wait(int)` adds its argument to `clock_gettime`'s `tv_sec` before
calling `sem_timedwait`; the scheduler's values are therefore seconds, not
milliseconds.  Its exact triggering rule is:

```text
on start: record the current last-AVMP-packet-received time
repeat until stopped:
  wait reconnect_check_timeout / 2 seconds
    (or one second after forceReconnect() posts its semaphore)
  elapsed = now - last-AVMP-packet-received
  attempt reconnect only when
    (elapsed > reconnect_check_timeout OR force flag is set)
    AND the stored availability argument is true
    AND AVMPSession::isReconnecting() is false
```

The strict comparison is important: equality does not trigger a normal
attempt.  `forceReconnect()` only sets the force flag and posts the wake
semaphore; it does not itself open a socket.  A terminal return from
`AVMPSession::reconnect()` is passed to the thread's virtual termination slot.
The scheduler also has a separate server-timeout terminal path that creates a
`VMSessionStatus` with status `DISCONNECT_REQUESTED (102)`, reason
`REASON_SERVER_TIMEOUT (7)`, timeout zero, and sends callback code `200`.
That path must not be relabelled as a successful reconnect.

`AVMPSession::reconnect()` performs the following ordered operations:

```text
pause packet receiver
acquire the connection semaphore
set library connection state VMAPI_RECONNECTING (-2003)
set AVMPSession::isReconnecting() true
set VMSessionStatus to RECONNECTING (103), mark it reconnecting,
  and send callback code 103 when a callback is installed
suspend the running receiver, sender, and reconnect workers
retry APCP/socket recovery until success, cancellation, or attempt timeout
on transport success: resume workers and queue an ordinary LoginMessage
on transport failure/cancellation: clear reconnecting, release semaphore,
  and return the terminal result to ReconnectThread
```

The attempt deadline uses the session's reconnect-attempt timeout and stops
only when `now - attempt_start > timeout`; again, equality is not the timeout
condition.  Between unsuccessful socket attempts the native client calls
`ASleepSec(5)`.  A cancelled or failed attempt does not silently continue as
an attached media session.  On the non-cancelled failure branch the client
sets `VMSessionStatus::RECONNECT_FAILED (104)` and invokes callback code
`104`; a replacement must surface an equivalent terminal feature fault.

For each attempt it destroys the prior `ProtocolAPCP` socket and constructs a
fresh one from the stored connection configuration, then calls:

```text
ProtocolAPCP::getSocketReconnect(2, 2000, setup_selector, stored_session_id)
```

The second argument is passed as literal `2000`; this particular native
function does not consume it after receiving the ABI argument, so it must not
be reinterpreted as a wire field or a retry delay.  `setup_selector` is `1`
only when the session's stored selector equals one; otherwise it is `2`.
Inside `getSocketReconnect`, selector `1` calls
`sendSessionRequestReconnect` (the already documented APCP `0x0101` writer),
whereas every other selector calls the regular `sendSessionRequest` (`0x0100`
writer).  Both paths receive a fresh APCP setup response afterwards.  The
fourth argument is assigned to the APCP object's session-id slot before the
writer is selected.  Thus a reconnect is not universally an `0x0101` packet,
and an initial `0x0100` writer is not universally a zero-session packet.

Once `AVMPSession::getSocket()` is non-null, the native client resumes its
receiver/sender workers *before* it queues login.  It derives the
`LoginMessage` version from the newly negotiated AVMP major/minor pair,
loads the stored username/password, explicitly sets `preempt = false`, marks
the receiver as expecting a login response, and enqueues the normal
user/password `LoginMessage`.  `reconnect()` itself does not send either
`SessionIdRequest (0x0102)` or `OTPRequest (0x0103)` and does not select the
random-number or certificate login encoders.

The response processors define the actual completion boundary:

1. `RequestProcessor::process(LoginResponse*)` preserves the raw login
   status in `VMSessionStatus`.  A non-zero status calls the public session
   callback with code `200`; on the reconnect/reconnect-available branches it
   also calls `cancelReconnect()`.  It is therefore a terminal login outcome,
   not a reason to leave a browser media operation pending.
2. On status zero, VM style `0` sends `GetVDiskInfo`.  For non-zero styles,
   the native condition sends `GetVDiskInfo` during reconnect only for styles
   `1`, `2`, or `3`; style `4` is not included by that predicate.  It then
   starts the normal post-login workers.  If reconnect is available,
   `sendOTPRequest()` is called here, subject to the independently documented
   OTP guards.
3. `RequestProcessor::process(VDiskInfo*)` refreshes the remote-drive list.
   When `isReconnecting()` is true it releases/rebuilds the remote mappings
   and calls `AVMPSession::remapDrives()`.  Return `API_SUCCESS (-1000)` is
   the only branch that publishes `RECONNECT_COMPLETED (105)` through callback
   code `200`; every other return publishes `RECONNECT_FAILED (104)` through
   the same callback.  Only after that decision does it clear
   `isReconnecting()`.

Consequently, a compatible implementation may expose a transient
"reconnecting" state, but it must not announce recovered media merely because
APCP, TLS, or Login completed.  It must retain the previous mapped-drive
intent until discovery and remapping produce the documented terminal outcome.
The native trace formatter logs the account name during this flow; that is an
official-client defect and must not be reproduced in service logs.

The supplied iDRAC6 Java VM implementation has a different boundary.  Its
`VirtualMedia::open` establishes APCP/TLS/login once and starts
`ApplianceSession`; `ApplianceSession::run` performs discovery then a
synchronous AVMP dispatch loop.  Its exception/termination path unmaps local
drives and closes the socket.  There is no reconnect scheduler, session-id
recovery request, OTP request, or remapping recovery branch in the supplied
`VirtualMedia`, `ApplianceSession`, `PacketAPCP`, and `PacketAVMP` sources.
This proves only that the supplied iDRAC6 client does not provide the iDRAC8
native recovery mechanism; it does not prove that every iDRAC6 firmware lacks
any server-side reconnect capability.  A legacy backend must therefore treat
a dropped iDRAC6 VM session as terminal unless a separate, complete iDRAC6
recovery oracle is obtained.

#### Native `SessionClose` serializer/caller mismatch

**N** `AVMPSession::sendCloseConnection` constructs `SessionClose`, calls
`setSessionId(stored_session_id)`, and queues it on `SendThread`. The generic
sender then calls the packet's virtual `encodeHeader(buffer)` followed by
`encodePayload(buffer + 12)` and writes exactly `packet_length` bytes.

`SessionClose::encodeHeader` produces an APCP header with total 16 and type
`0x01ff`. Its payload encoder zeroes its four-byte argument, but then writes
the stored session id at *offset 12 relative to that payload argument*:

```text
SendThread payload pointer = packet_buffer + 12
SessionClose::encodePayload write offset = 12
actual write address = packet_buffer + 24
SendThread write length = 16
```

Thus this supplied native library's generic send path emits the sixteen-byte
record `APCP | u32be(16) | u16be(0x01ff) | u16be(0) | u32be(0)`; the id write
lands beyond the transmitted 16-byte allocation. This is a source-confirmed
serializer/caller mismatch, not a field interpretation. It also means the
earlier claim that this binary sends `session_id` as the four-byte close
payload was false. A replacement must not "repair" that byte in isolation:
it needs a separate, documented compatibility decision and a hardware result
for the target profile. Until then, preserve the official lifecycle ordering
but treat the close payload as the observed all-zero native record.

#### Native VM packet-factory inventory and unsupported boundary

`PacketReceiver::createPacket` statically recognizes the packet types below.
The first group is required by the repository's readonly ISO path; the second
group is catalogued only to prevent it being mistaken for an unknown packet or
silently handled by a generic decoder.

| Type | Official native packet class | Repository policy |
|---:|---|---|
| `0x8100` | `LoginResponse` | required |
| `0x8200` | `VDiskInfo` | required |
| `0x8300` | `ReadRequest` | required |
| `0x8410` | `DeviceStatus` | required |
| `0x8240` | `VDiskRelease` | release acknowledgement/notification; clear matching local state without sending `ClientStatus` |
| `0x0400` / `0x8400` | `Heartbeat` | required when the official heartbeat branch is active |
| `0x8110` | `Disconnect` | terminal session fault/cleanup |
| `0x8310` | `VDiskWrite` | parser and official write path recorded; readonly backend must fault before a local write |
| `0x8320` | `VDiskGetAlternateTocData` | parser and response-selection path recorded; outbound response is native-buggy and unsupported |
| `0x8430` | `DeviceConfigurationOption` | parser and option-1 state effect recorded; no client sender is exposed |
| `0x8241` | `SetDeviceCountResponse` | raw counter parser recorded; unsupported configuration branch |
| `0x8500`, `0x8530`, `0x8540`, `0x8570` | server-image response classes | layouts and native manager transitions recorded; product feature unsupported |
| `0x8102` | `SessionIdResponse` | validated iDRAC8 runtime input; id remains private, timeout updates recovery cadence, and successful recovery still requires rediscovery/remap |
| `0x8103` | `OTPResponse` | layout and state transition recorded; product branch remains unsupported pending complete OTP launcher/response policy evidence |

This table is a receive inventory, not permission to emit the unsupported
types. An unimplemented recognized type must have a deterministic outcome;
it must not be ignored while the frontend remains in a pending state.

#### Native server-image records and upload state machine

The native VM library contains a separate server-image facility. It is not a
variation of `VDiskRequest`, and its indexes live in the AVMP header extension
rather than in the ordinary remote-drive namespace. All facts in this section
are **N** from the named native serializers, parsers, `ServerImageManager`,
and `ServerImageUploadThread`.

| Type | Direction | Header extension | Deterministic wire contract |
|---:|---|---|---|
| `0x0500` | client | zero | `ServerImageInfo`, total 12, no payload |
| `0x0530` | client | store id | `ServerImageSpace`, total 12, no payload. The supplied caller uses store `0x0010`. |
| `0x0550` | client | image index | `ServerImageDelete`, total 12, no payload |
| `0x0540` | client | image index | `ServerImageCreate`, declared total 280; fixed fields and its non-deterministic remainder are stated below |
| `0x0570` | client | image index | `ServerImageData`, total `18 + data_len`; payload `offset:u32be | data_len:u16be | data:[data_len]` |
| `0x8500` | BMC | entry count | `ServerImageInfoResponse`; exactly one 268-byte record per count |
| `0x8530` | BMC | store id | `ServerImageSpaceResponse`, payload `total:u32be | free:u32be` |
| `0x8540` | BMC | image index | `ServerImageCreateResponse`, payload `status:u8` |
| `0x8570` | BMC | image index | `ServerImageDataResponse`, payload `status:u8` |

`ServerImageInfoResponse::decode` treats the header extension as the number
of following entries.  Each entry is exactly:

```text
index:u16be | vdisk:u16be | size:u32be | flags:u32be |
type:u8 | format:u8 | store:u8 | name:[253]
```

The official parser advances by 268 bytes per entry without checking that the
advertised packet contains them, and passes the fixed 253-byte name region to
a C-string setter without first proving an in-region NUL.  A replacement must
require `payload_len == 268 * extension_count` (or reject a documented,
bounded extension before parsing it) and decode the name as a bounded byte
region.  It must not inherit either out-of-bounds read.

`ServerImageCreate::encodePayload` does **not** produce a complete stable
268-byte payload.  With offsets relative to its payload pointer, the official
writer advances over bytes `0..3` without storing them, writes the following
fields, then performs an unbounded `strcpy` from the image name at offset 15:

```text
0..3    untouched by this serializer
4..7    size_in_blocks:u32be
8..11   flags:u32be
12      type:u8
13      format:u8
14      store:u8
15..    NUL-terminated name written by strcpy
```

`SendThread` allocates its output buffer with `new[]`, calls the header
writer (which clears only bytes `0..11`), then calls this payload writer.  It
does not initialise the skipped bytes or the name tail.  The emitted bytes
outside the fields above are therefore heap-dependent, and a long name can
overflow the 280-byte message allocation.  There is no reproducible official
byte oracle for that remainder.  A safe replacement must not emulate memory
disclosure: it needs an explicitly versioned, zero-filled bounded-create
contract validated against target hardware before this feature can be enabled.
Until then, `0x0540` is parser-known but sender-unsupported.

The unstripped official Linux binary independently confirms this conclusion:
`ServerImageCreate::encodePayload` at `libVMAPI_DLL.so+0x000d_c998` advances
the destination by four bytes before its first `encodeInt`, then calls
`strcpy@plt` at `+0x000d_ca8f` with no capacity argument. Its constructor
declares packet length `0x118` (280) at `+0x000d_c86f`. These are direct
machine-code facts, not a decompiler naming inference.

`ServerImageData` is well-defined: its constructor stores an `u16` length,
sets the packet length to `18 + length`, writes the header extension, then
writes offset, length, and exactly that many source bytes.  The manager waits
up to 30 seconds for an event code `3` after each data request.  A
`0x8570` response supplies the event's index and raw status; the native
waiter wakes on event identity rather than checking that status for success.
That status must be preserved and a non-zero value treated as a terminal
upload fault by any safe implementation.

The source-confirmed native lifecycle is:

```text
ServerImageInfo / ServerImageSpace (independent list and capacity queries)
  -> ServerImageCreate(index, metadata) -> 0x8540
  -> zero or more ServerImageData(index, offset, bytes) -> 0x8570
  -> one zero-length ServerImageData(index, 0, 0) -> 0x8570
  -> optional ServerImageDelete(index)
```

`ServerImageManager::imageCreate` waits for event `2`, returns the response
index, and treats a non-zero response status as failure.  Its uploader reads
the local source in 64 blocks of 512 bytes (`32768` bytes) at a time, sends
that byte count as the `u16` data length, advances its offset by 64, and
finishes with the zero-length data record.  It can request delete after a
cancel or a zero-length terminal transfer.  These are the official client's
state transitions; they do not establish the product semantics of `type`,
`format`, `flags`, or `store` values.  The upload path also has no received
status check for data responses, so its control flow must not be copied as a
success criterion. The backend has bounded codecs for `0x0500`, `0x0530`,
`0x0550`, deterministic `0x0570`, and all four response layouts.  These are
not a registered product feature: `0x0540` remains deliberately unavailable
because the official create serializer has no deterministic safe byte form.

The surrounding upload setup is also source-confirmed.  In
`ServerImageUploadThread::run` the client creates the `VMServerImage` with
`flags=0` and `format=0`; it chooses `type=1` for a file-backed CD image and
`type=2` for a file-backed remote disk.  The supplied
`RP_VMAPI_LIB::uploadServerImage` checks the server-image inventory before
that step and returns without emitting `0x0540` when the list is nonempty.
This is a client-side precondition, so a safe implementation must query the
inventory first and refuse the create operation under the same condition.

For the iDRAC8 interoperability fixture, a direct hardened `0x0540` probe
against a nonempty list was rejected with response extension `0` and payload
`[1, name..., 0]`.  `ServerImageCreateResponse::decode` at
`libVMAPI_DLL.so+0x000d_cc56` consumes only the header extension and payload
byte zero; the trailing name bytes are not a response field.  Consequently,
status `1` is preserved as an opaque BMC failure status, not decoded as a
success or a name echo, and no retry is permitted without a new
source-confirmed precondition.

#### Native writable-media, alternate-TOC, and configuration records

These records are part of the supplied iDRAC8 VM receive factory. The native
backend now has bounded codecs for `VDiskWrite(0x8310)` and
`DeviceConfigurationOption(0x8430)`: option values are retained raw per
remote disk, and only option id `1` updates the source-confirmed
remote-readonly state. Writable mapping remains disabled; having a parser is
not authority to write a local ISO.

`VDiskWrite(0x8310)` has the following BMC-to-client layout:

```text
header extension  remote_disk_id:u16be
payload 0..3      offset:u32be
payload 4..7      block_count:u32be
payload 8..       data (declared packet length minus 20 bytes)
```

The native parser copies all remaining bytes, but its subsequent write path
uses `block_count * local_block_size` instead of the copied data length.  It
looks up a locally mapped, writable drive, adds its LBA start to `offset`,
writes that many bytes, and then sends `ClientStatus(0x0410)` with a raw
status.  It does not verify that the two lengths agree.  A safe implementation
must first require checked multiplication and exact equality
`data_len == block_count * negotiated_local_block_size`; absent that proof it
must reply/terminate according to an explicit writable-media policy, never
write a local file.  The existing readonly backend therefore remains required
to fault this message before any local write.

The same official binary shows that `VDiskWrite::encodePayload` at
`libVMAPI_DLL.so+0x000d_9784` is an empty function (prologue then `leave;
ret`). `VDiskWrite` is therefore an inbound BMC write request plus native
local-drive handler, not evidence of a client-side generic writable-media
serializer. No product feature may infer an outbound write wire form from it.

`VDiskGetAlternateTocData(0x8320)` is a BMC request, not data.  Its header
extension is the remote-disk id and its three-byte payload is, in this exact
order, the three raw one-byte values returned by native getters
`getMSF()`, `getFormat()`, and `getTrack()`.  `RequestProcessor` forwards
them to `sendVDiskAlternateTocData`.

The intended response class is `VDiskAlternateTocData(0x0320)`: it stores
the same remote-disk id in the header extension, allocates a TOC byte array,
and nominally declares total length `14 + toc_len`.  However, the official
serializer writes `toc_len:u16be` at payload offset 10 and copies the TOC at
payload offset 12, while `SendThread` transmits only `packet_length` bytes
and passes `packet + 12` as the payload pointer.  Consequently the declared
payload length is only `2 + toc_len`; bytes `0..9` are uninitialised, and the
TOC copy begins ten bytes beyond the transmitted allocation.  This is a
confirmed native serializer/caller mismatch, not an alternative TOC layout.
No deterministic compatible outbound wire form follows from this artifact.
The alternate-TOC response must stay unsupported until a target-specific
official implementation or hardware evidence supplies a safe contract.

`DeviceConfigurationOption(0x8430)` is BMC-to-client and has:

```text
header extension  remote_disk_id:u16be
payload 0..3      option_id:u32be
payload 4..7      value_width_tag:u32be
payload 8..       value encoded from width tag
```

The supplied parser recognizes width tags `1`, `2`, and `4`; it reads,
respectively, an unsigned byte, u16be, or u32be at payload offset 8.  Other
width tags leave its data slot at zero.  The only recovered state use is
`option_id == 1`: it treats `value != 0` as a remote-readonly flag and applies
it to every known remote drive.  No other option id is assigned a product
meaning by this source.  A replacement must validate the payload length from
the width tag and retain unknown option ids raw; it must not manufacture a
configuration response (the supplied native artifact has no corresponding
client encoder).

`SetDeviceCount(0x0240)` is conditionally emitted by
`AVMPSession::sendSetDeviceCount`.  It first unmaps all drives when all three
caller counts are zero, and only includes its four-byte payload if the native
session predicate `isAVMPVersion12()` is true:

```text
type 0x0240, total 12                  when predicate is false
type 0x0240, total 16                  when predicate is true
header byte 10                         (count_2 low byte << 4) |
                                       ((count_1 + count_3) & 0x0f)
header byte 11                         0
payload byte 0                         untouched by the encoder
payload byte 1                         count_3 low byte
payload byte 2                         count_2 low byte
payload byte 3                         count_1 low byte
```

The paired `SetDeviceCountResponse(0x8241)` reads raw full-record bytes 13,
14, and 15 (payload bytes 1, 2, and 3) into its three returned counters.  It
does not consume a status code and its processor only produces a session
callback; no media transition is driven by that response in the supplied
artifact. The hardened sender clears the untouched payload byte and retains
the three official semantic arguments (`numFloppy`, `numCd`, `numFlash`).

The tested iDRAC8 firmware advertised two type-zero/status-disabled
placeholders with capability bit zero set. An earlier probe observed a
`DeviceStatus(0x8410)` followed by `VDiskInfo(0x8200)` after sending
`SetDeviceCount`, but that ordering does not establish causality: the official
processor routes `DeviceStatus` through per-drive map/error handling, while
only `SetDeviceCountResponse(0x8241)` releases the device-count API waiter.
Consequently `0x8410` must never complete or reject `SetDeviceCount`, and
disabled placeholders must never be used directly as map targets. The tested
iDRAC8's two placeholders correspond to the optical/floppy devices also seen
in its Avocent USB composite enumeration. On an explicit optical or floppy
selection, the backend therefore uses the native API's `1 floppy, 1 CD,
0 flash` layout. `0x8241` remains the native API waiter's response, but the
tested firmware omits it and instead publishes a new `VDiskInfo` containing
idle type `2` and type `1` slots. The backend accepts that independently
verified table convergence without treating the intervening `0x8410` as an
acknowledgement, then maps the generation-compatible slot. This mutation is
not sent during login, close, reconnect, or ordinary status polling.

A hardware run of the exact extended `1 floppy, 1 CD, 1 flash` request also
returned `DeviceStatus(0)` and then the same two-entry type `2 + 1` table. The
type-1 target accepted a `VDiskRequest2` flash map and issued 14 read requests
for 98 blocks, while the type-2 target independently served optical reads.
Attempting a floppy map at the same time was correctly rejected because no
second removable target existed. Thus the three counters select device roles;
they do not prove that firmware will instantiate three independent USB targets.
The type-1 target is shared by floppy and flash emulation on this firmware.

`VDiskRequest2(0x0211)` is a distinct fixed 26-byte AVMP request with a
remote-disk header extension and this 14-byte payload:

```text
0..3    block_size:u32be
4..7    capacity:u32be
8       drive_flags:u8
9       request_block_limit:u8 (official sender clamps only values above 255)
10..11  cylinders:u16be
12      heads:u8
13      sectors:u8
```

The class is selected for a floppy-geometry branch; it is not a compact CD
mapping request.  Its decoder has asymmetric byte-to-integer transforms for
the one-byte fields, another reason not to use a decoded instance as a wire
specification.  The serializer above is the authoritative outbound contract.
The iDRAC8 `AVMPSession::sendMapRequest2` path reads block size, block count,
cylinders, heads, sectors, readonly state, and request limit from the local
drive, constructs this request, and queues it through the normal map path.
Together with `VMRemoteDrive::REMOTE_DRIVE_IS_FLOPPY = 4`, this establishes
the iDRAC8 readonly floppy sender and drive-selection lifecycle.
Native `isCompatibleDriveType` additionally accepts remote removable type `1`
for local floppy types `4` and `6`; the generation-specific backend mapping
therefore accepts either iDRAC8 remote type `1` or `4` for a floppy image.

#### iDRAC6 Java floppy and smart-card corroboration

The supplied iDRAC6 Java VM client corroborates the native wire layout and
provides a second generation's lifecycle. The following is **S** from
`com.avocent.vm.PacketAVMP` and `ApplianceSession`; it establishes packet
geometry only. It does not authorise enabling physical floppy or smart-card
access in the backend.

`ApplianceSession::cmdMapFloppyDrive` selects `VDiskRequest2(0x0211)` with
total length 26 and passes the local drive's block size, number of blocks,
read-only flag, cylinders, heads, and sectors. `PacketAVMP` serialises those
values exactly as the iDRAC8-native layout above. It selects only an
advertised remote drive whose Java `DriveInfo` type is `1` and status is `0`.
The iDRAC6 map lifecycle is therefore:

```text
VDiskInfo(type 1, status 0)
  -> VDiskRequest2(0x0211, geometry payload)
  -> DeviceStatus(0x8410)
  -> normal ReadRequest(0x8300) / ReadResponse(0x0300) and,
     when writable, VDiskWrite(0x8310) / ClientStatus(0x0410)
  -> VDiskRelease(0x0220)
```

`ApplianceSession::cmdUnmapDrive` queues `VDiskRelease(0x0220)` and returns.
The input worker dispatches decimal `33344` (`0x8240`) to
`responseReleaseMapping`, which only returns the receive buffer and schedules
the local mapping cleanup callback. It does **not** send `ClientStatus`.
`ClientStatus` is reserved for read/write and smart-card transfer completion.
The tested iDRAC6 stops publishing its type-1 removable target after release
until the AVMP media connection closes. The backend therefore recycles the
iDRAC6 AVMP session after the final mapping is removed, while retaining the
session when an independent optical mapping remains. iDRAC8 does not use this
generation-specific recycle policy.

ServerImage inventory responses (`0x8500` and `0x8530`) are recognised manager
records, including when they arrive after a bounded catalog query completes.
They update the cached catalog/space state and never terminate the readonly
media session. A mount-specific capability refresh only requests VDiskInfo;
the independent persistent ServerImage/vFlash probe is excluded from the map
path.

The supplied Java `ApplianceSession::getVDiskInfoAll` sets its current
`VDiskInfo` to null, sends one `GetVDiskInfo`, and waits in 100 ms increments
for up to `OptionsFile`'s default 30-second response window. Its
`responseVDiskInfo` replaces the current `DriveInfo[]` with the received array;
the official JAR does not contain a 20-request/40-ms retry loop. The tested
iDRAC6, however, can return optical-only, then removable, then the complete
table across successive requests. The backend therefore applies a bounded
20-request/40-ms, iDRAC6-only compatibility policy during initial/requested
discovery. Those bounds are an empirical product policy, not a recovered wire
requirement. No device-count mutation is sent on this path.

The supplied JAR's `ApplianceSession::mapDrive` then keeps the `DriveInfo[]`
returned by its immediately preceding `getVDiskInfoAll`, selects the idle
remote index from that array, and sends the map request in the same method. A
tested iDRAC6 can emit a shorter unsolicited `VDiskInfo` immediately after the
requested table. Because the generic feature layer resolves the selected
image between refresh and map, the backend retains that operation's verified
drive snapshot and consumes it once at map dispatch. It still records the
later table as current status, but does not revalidate the in-flight operation
against it. A failed refresh clears the snapshot, transport recovery clears
all snapshots, and a map without a preceding requested refresh falls back to
the current table.

Hardware acceptance also established that iDRAC6 may hide an idle sibling
target from later `VDiskInfo` responses while optical media remains mapped.
For example, a session that first advertised idle type `2 + 1` subsequently
advertised only mapped type `2`, even though its type-1 removable target was
still usable. The backend therefore retains only drive IDs and types that the
BMC has actually advertised during the current authenticated AVMP session.
When the requested target is absent from the latest short table, map dispatch
may use that session-verified topology only if the target is not already
occupied by a local mapping. Reconnect discards the topology; it is never
guessed or carried across sessions. The resulting map is still considered
successful only after the BMC returns a successful `DeviceStatus`, so the
retained topology does not replace the protocol's authoritative result.

This confirms that the geometry form is a map request, not a passive drive
description. The Java client permits writes only because it opens a local
physical medium through its native host library and handles `0x8310`; a
server-side readonly media feature must continue to reject that write request
with a structured fault rather than borrowing the floppy map record.

The iDRAC6 smart-card map request is a separate AVMP family:

```text
0..3    "AVMP"
4..7    total = 14 + atr_len
8..9    type = 0x0213 (VCardRequest)
10..11  remote_disk_id:u16be
12..13  atr_len:u16be
14..     ATR bytes
```

`ApplianceSession` maps this only to an advertised Java drive type `8` with
status `0`, first obtains the local smart-card ATR, then emits `0x0213`.
The BMC's `VCardXferBlock(0x8301)` uses the same variable payload form with
`apdu_len:u16be` at bytes `12..13` and APDU bytes at 14. The client transfers
that APDU to its local smart-card library and returns
`VCardDataBlock(0x0301)`, again `14 + response_len` bytes with the remote id
and `response_len:u16be` followed by response bytes. It then sends
`ClientStatus(0x0410)` status `0` on success or status `2` on transfer
failure.

The official Java parser trusts the u16 ATR/APDU lengths before copying. The
native backend now provides bounded, iDRAC6-scoped codecs for all three packet
forms, but does not register a smart-card feature. A replacement must validate
`14 + declared_len <= total_length` before allocation/copying. The supplied iDRAC8 native VM packet factory does not
establish a corresponding smart-card send/receive lifecycle, so smart-card
support remains intentionally disabled for iDRAC8 and iDRAC7; this iDRAC6
record is evidence for a future separately profiled legacy implementation,
not a generic AVMP fallback.

`UsbReset(0x0420)` is fully serialised by the iDRAC8 native VM library:
total 16, AVMP header extension zero in the official no-argument send path,
and four zero payload bytes.  `AVMPSession::sendUsbReset` only checks its
stopping flag before queuing the request; the supplied receive factory has no
corresponding typed acknowledgement or completion transition.  It is thus a
known one-way control record, not an operation a backend may report as
successful without target-specific completion evidence.

#### AVCT implementation alignment status

The following source-to-source discrepancies have been removed and are locked
by offline byte/state oracle tests: typed selection of all three APCP initial
writers; branch-specific APCP response parsing; profile-selected `0x0100`,
`0x0102`, and `0x0104` login encoders; both login response layouts with the
numeric status preserved; direct video opening without the fabricated control
barrier; dual-random `Video Channel Auth`; and iDRAC6/8-specific inbound
packet limits.

This does **not** make every AVCT path operational. The backend must still
surface an explicit unsupported-protocol fault rather than guess when it
encounters: a reconnect transition requiring retained `Fb`; an APCP `0x8103`
credential-material branch; a capability-conditioned NextBoot/OTP record;
or a display bootstrap branch whose scaling/certificate predicates have not
been represented. A TCP/TLS connection alone remains insufficient evidence of
an operational console.

#### Native VM implementation alignment status

The readonly AVMP backend now has byte-oracle coverage for the native iDRAC8
login total/layout, its versioned `0x0104` form, generation-scoped iDRAC6
fixed-99-byte versus iDRAC8 variable login responses, iDRAC6 versus iDRAC8
APCP initial writers, generation-specific discovery extensions, `ReadRequest`
chunking limit, `DeviceStatus == 0` mapping success, `ClientStatus`, and the
all-zero native `SessionClose` record. The runtime rejects an omitted or
unsupported generation profile rather than conflating iDRAC6 Java and iDRAC8
native formats.

It is still not a complete AVMP implementation: random/certificate/OTP
credentials, writable media, alternate TOC, smart-card host integration, and
server-image upload remain disabled until each documented state transition has
its own oracle and typed fault outcome. iDRAC8 direct-account reconnect and
the configured heartbeat path are implemented with source-confirmed recovery
and remap boundaries; this does not enable the separately guarded OTP flow.
The protocol layer has byte-oracle codecs for iDRAC6 alternate random/capability
login, iDRAC6 smart-card records, native server-image info/space/delete
requests, writable/configuration inbound records, and retained raw device
options; none of those codecs alone exposes a new feature operation.
Client-initiated unmap follows native `unmapLocalDrive`: clear local mapping
state and queue `0x0220` without waiting synchronously for a response. Incoming
`0x8240` is the terminal release acknowledgement/notification; its header disk
id selects the mapping to clear, and the official receiver sends no
`ClientStatus`. iDRAC7 cannot be inferred from iDRAC6/8 and remains unsupported
without an official iDRAC7 client artifact.

### iDRAC6 Java VM corroboration

#### iDRAC6 AVCT console boundary

The supplied iDRAC6 KVM JAR has its own complete receive factory
(`com.avocent.kvm.b.a.a`). It shares the normal eight-byte `BEEF` framing and
many message numbers with the iDRAC8 client, but it is not evidence that all
modern branches are accepted by iDRAC6:

| Static difference | iDRAC6 source fact | Implementation rule |
|---|---|---|
| Minimum AVCT record size | Its factory rejects a declared total length below 16 and above 15,000 before constructing a message. | Keep iDRAC6 bounds profile-specific; do not borrow the iDRAC8 32,768 receive limit as a wire guarantee. |
| Display selectors | Its factory maps `0x0081/0x0082/0x0083` and `0x8601/0x8602/0x8603` to the DVC envelope. It has no `0x008a/0x860a` factory cases. | Do not advertise DVC23 on iDRAC6 from the iDRAC8 source alone. |
| Modern control surface | Its factory lacks the iDRAC8-only receive branches catalogued for sharing, custom-server, protocol-option, and later extended display families. | A common Rust enum may contain the numbers, but an iDRAC6 runtime profile must reject unproved branches deterministically. |
| Direct console login | `com.avocent.kvm.b.a.y` uses fixed legacy account layouts; its `0x0100`/`0x0102` behaviour does not establish iDRAC8 `0x0104` acceptance. | Select login from the negotiated profile and generation-specific evidence, never from a backend name. |
| Console transport | `kvm.b.p` has two launcher paths. Its default `USE_APCP=false` is direct TLS, while the supplied Dell launcher passes `apcp=1`, selecting `k.a/k.b`: 53-byte `APCP 0x0100/version=0`, protocol bytes `0,0`, then negotiated TLS on both channels. Both paths use fixed version-2 `0x0102`; `yb` serialises video auth as `BEEF 0x0001 | client_random:u32be | appliance_random:u32be`. | The configured iDRAC6 profile follows the supplied `apcp=1` launcher exactly. It does not use iDRAC8's 2.41 APCP request, APCP heartbeat/close record, or reconnect writer. |

This table is intentionally restrictive: absence from the iDRAC6 factory is
not a claim that a particular firmware can never send a value, only proof
that the supplied official iDRAC6 client has no parser for it. A native backend
must surface that mismatch rather than inheriting iDRAC8 behaviour.

For completeness, this is the full explicit switch closure of that supplied
iDRAC6 factory. The middle column deliberately gives the iDRAC6 class rather
than borrowing a semantic label from the iDRAC8 package. A grouped row means
that the same iDRAC6 factory branch selects the listed class for every type in
the group.

| iDRAC6 incoming type(s) | iDRAC6 receive class | Profile boundary |
|---:|---|---|
| `0x0000`, `0x0001` | `ab`, `yb` | acknowledged video / video-channel-auth response classes |
| `0x0080` | `db` | display-control class |
| `0x0081/0x0082/0x0083`, `0x8601/0x8602/0x8603` | shared `fb` instance | DVC display profile; no `0x008a/0x860a` branch exists |
| `0x0084`, `0x0085`, `0x8605` | `vb`, `v` | video connect/stop inputs |
| `0x0086`, `0x8606` | `eb` | block-video class, distinct from the DVC family |
| `0x0087/0x0088/0x0089`, `0x8607/0x8608/0x8609` | `sb`, `gb`, `ib` | auxiliary display classes; no `0x008b/0x860b` branch exists |
| `0x0100`, `0x0101` | `y`, `z` | protocol/control records |
| `0x0201`, `0x0204` | `ec`, `g` | mouse and keyboard-LED classes |
| `0x0302`, `0x0304`, `0x030e` | `w`, `o`, `bb` | display setup/control classes |
| `0x0400`, `0x0600` | `p`, `ab` | keepalive / class-specific control input |
| `0x0424` | `bc` | variable legacy outbound/receive class: `string_len:u16be, raw string bytes, raw byte`; product values for that final byte are not recovered |
| `0x8100/0x8101/0x8102` | `kb`, `nb`, `mb` | HID status/acknowledgement inputs |
| `0x8200`, `0x8201`, `0x8202`, `0x8203`, `0x8204`, `0x8206` | `jb`, `hb`, `wb`, `rb`, `l`, `jb` | resolution/setup/colour inputs |
| `0x8300`, `0x8301`, `0x8302`, `0x8304`, `0x8305` | `tb`, `x`, `pb`, `q`, `tb` | login/control family |
| `0x8401` | `cc` | sharing request class; the supplied factory has no `0x8400` branch |
| `0x8412`, `0x8413` | `lb`, `qb` | restricted control responses |
| `0x8420` | `ub` | restricted control response |
| `0x8430`, `0x8431`, `0x8432`, `0x8434` | `c`, `zb`, `ac`, `j` | chat/credential/server-status family |
| any other type | `ob` fallback | official no-op fallback; a native backend must instead return an unsupported-protocol fault |

The source contains no iDRAC6 factory cases for `0x008a/0x008b`,
`0x8103`, `0x8205`, `0x8410/0x8411/0x8414/0x8415`, `0x8435`, or the modern
`0x9e**`/`0x9f00` families. This closes the supplied artifact's receive
surface without asserting that those values are impossible on every iDRAC6
firmware.

`com.avocent.vm.PacketAVMP`, `ApplianceSession`, and `VDiskInfo` from the
iDRAC6 client independently confirm the same header geometry, later
remote-drive extension use, readonly request types, and
`type/status/capabilities` drive-entry layout. They also show a
`0x0200 -> 0x8200` discovery before any map, request chunking by the third
`0x8300` u32, and a `0x0410` client status after read processing. The iDRAC6
discovery request uses extension `0x0000`, whereas the recorded iDRAC8 native
writer uses `0xffff`; this is a confirmed generation-specific difference.

This is corroborating evidence only. It does not permit iDRAC6-specific
login, TLS, reconnect, physical-drive, writable-media, smart-card,
alternate-TOC, or configuration-option behavior to be enabled for the native
iDRAC8 implementation without a corresponding native symbol and state-path
record.

#### iDRAC6 VM transport is a separate profile

The iDRAC6 Java VM client contains a complete, independently implemented
APCP/login path. It establishes several **S** differences from the iDRAC8
native VM engine above:

| iDRAC6 Java writer | Exact record | Consequence |
|---|---|---|
| `VirtualMedia#sendSessionRequest` | 53 bytes: `APCP`, total `53`, type `0x0100`, extension `0`, protocol byte, two version bytes, zero, capability `u32`, random length, then a 32-byte padded random region | It is not the iDRAC8-native 68-byte initial request. |
| `VirtualMedia#sendSessionRequestBarcelona` | The same record, total `55`, with literal `u16be(3668)` inserted before the random-length byte | This is a distinct source-selected writer, not a generic extension field. |
| `VirtualMedia#receiveSessionSetup` | Requires marker `0x8100`; reads the fixed prefix through the padded random field and exposes major/minor, capability vector, redirect port, and random auth bytes | It does not justify parsing iDRAC8's conditional 68-byte APCP tail with the iDRAC6 layout. |
| direct Web-account login | AVMP `0x0100`, total `153`, fixed layout already recorded for the native legacy form | This is the iDRAC6 user/password branch. |
| random-login branch | AVMP `0x0102`, total `22`: two u32 random values, a raw zero byte, then the preempt byte | It is distinct from user/password login and is selected only when either launcher random is non-zero. |
| capability-8 login branch | AVMP `0x0101`, total `12`, empty payload | `VirtualMedia.main` selects it after receiving setup capability value `8`; it is not a generic reconnect request. |

The iDRAC6 writer gives the direct-login response an independently confirmed
parser boundary. After the 12-byte AVMP header (`type = 0x8100`, extension
ignored by this parser), `VirtualMedia#receiveLoginStatus` reads:

```text
payload 0       status:u8
payload 1       flags:u8; only bit 0 is exposed (`canRejectPreemption`)
payload 2       username_length:u8
payload 3..     username bytes
then            zero padding consumed until the complete username region is 96 bytes
```

The source always consumes this 99-byte payload shape after its header and
does not use the declared AVMP total to shorten it. A compatible defensive
parser must validate the full fixed region before reading it; it must not copy
the official client's unbounded allocation behaviour.

The launcher predicates are also exact in this artifact: the `0x0102` writer
is selected when either supplied random value is non-zero; otherwise the
`0x0100` direct Web-account branch is selected. The temporary
`random_auth || username || password` concatenation constructed immediately
before the direct branch is not passed to its serializer; that serializer is
called with the raw Web password bytes. The no-argument `0x0101` writer is
selected only by the setup-capability-8 branch above. Therefore zero randoms
are not a reason to fall back to `0x0102` or `0x0101`.

For the selected iDRAC6 `0x0102` path, `VirtualMedia#login(int,int,byte,byte)`
writes the ten-byte payload exactly as `client_random:u32be`,
`appliance_random:u32be`, raw byte `0`, and `preempt:u8`. The caller passes
that third argument as literal zero. It is not a port field; iDRAC6's direct
account branch carries its appliance port and channel elsewhere in its fixed
`0x0100` payload. This matches the native iDRAC8 random-login byte geometry,
but the branch-selection predicates remain profile-specific.

The iDRAC6 Java main path chooses a connection capability value then dispatches
only `4` into its JSSE TLS wrapper and `8` into a separate vendor transport
wrapper. The bit names are not a portable protocol vocabulary. This confirms
the rule already applied to the iDRAC8 native/AVCT branches: select only the
exact transport code the profile implements and fault every other negotiated
value.

Its `ApplianceSession` shutdown path unmaps locally mapped drives and closes
the socket. The supplied iDRAC6 Java source does not construct the iDRAC8
native `SessionClose(APCP 0x01ff)` object. That absence is a
generation/profile difference, not authority to omit the native iDRAC8 close
lifecycle record from an iDRAC8 implementation. The iDRAC8 native object's
serializer/caller mismatch is documented above and is not evidence for an
iDRAC6 session-id field.

#### Complete AVMP type closure in supplied official artifacts

`PacketAVMP` in the supplied iDRAC6 client is the source of the static labels
and constants below. The iDRAC8 native `PacketReceiver::createPacket` names
the overlapping receive cases. A row marked unsupported is still part of the
recovered protocol closure; it is not permission for the readonly backend to
emit, drop, or reinterpret it.

| Type | Official static class/label | Direction established by source | Repository boundary |
|---:|---|---|---|
| `0x0200` | Get VDisk Info | client request | readonly discovery, generation-specific extension |
| `0x0210` | VDisk Request | client request | readonly CD/ISO mapping only |
| `0x0211` | VDisk Request2 | client request | readonly floppy geometry mapping for generation-specific idle slots |
| `0x0213` | VCard Request | client request | smart-card; unsupported |
| `0x0220` | VDisk Release | client request | required normal unmap |
| `0x0230` | VDisk Set Enable | client request | unsupported |
| `0x0240` | Set Device Count | client request | explicit native API codec only; not emitted by map/close lifecycle |
| `0x0300` | VDisk Read Data | client response | readonly read response |
| `0x0301` | VCard Data Block | client response | smart-card; unsupported |
| `0x0320` | VDisk Alternate TOC Data | client response | unsupported |
| `0x0400` | Heartbeat | client record | profile-gated lifecycle requirement |
| `0x0410` | Client Status | client response | required after every readonly read outcome |
| `0x0420` | USB Reset | client request | unsupported |
| `0x0430` | Client Configuration Option | client request | unsupported |
| `0x8110` | Disconnect | BMC record | terminal feature fault and cleanup |
| `0x8120` | Disconnect Cancel | BMC record | unsupported until its session policy is ported |
| `0x8200` | VDisk Info | BMC response | required discovery parser |
| `0x8240` | VDisk Request Release | BMC response | typed remote-release state input |
| `0x8241` | Set Device Count Response | BMC response | sole source-confirmed completion of the explicit native count API |
| `0x8300` | VDisk Read | BMC request | readonly read servicing, chunked by third u32 |
| `0x8301` | VCard Transfer Block | BMC request | smart-card; unsupported |
| `0x8310` | VDisk Write | BMC request | readonly backend must fault, never write local data |
| `0x8320` | VDisk Get Alternate TOC Data | BMC request | unsupported |
| `0x8410` | Device Status | BMC response | required map outcome; zero is success |
| `0x8430` | Device Configuration Option | BMC response | unsupported |

The iDRAC8 native factory additionally contains server-image response types
`0x8500`, `0x8530`, `0x8540`, and `0x8570`, whose serializers, parsers, and
native manager lifecycle are recorded above.  The create and alternate-TOC
serializers contain source-confirmed uninitialised/overflow output defects,
and data-response status is not checked by the official waiter.  These
branches remain terminally unsupported rather than being hidden behind the
generic AVMP packet parser.

## Required follow-up before a hardware claim

1. Encode the three recorded APCP request writers and their selected bounded
   response parser as a typed profile/state machine; select the writer from
   negotiated/profile state rather than a constant.
2. Select `0x0100`/`0x0102`/`0x0104` from the negotiated APCP version, and
   implement both `0x8300`/`0x8305` response layouts plus every known
   non-success login result.
3. Remove the artificial attach barrier and unconditional APCP post-attach
   writes. Gate capability/reconnect/OTP records on the exact parsed branch.
4. Represent `0x0084`, video attach, and the `C()` scale/geometry choices as
   an explicit state machine. The initial profile must be an evidence-backed
   direct branch, not the iDRAC6 fallback triple.
5. Implement the official acknowledgement counter or constrain support to the
   source-confirmed default `R = 1`; do not use a semantic-free magic byte.
6. Cover all eight known display types. Unsupported types must cause an
   immediate structured video-source fault, including the type and no frame
   data, rather than leave a browser waiting.
7. Preserve the verified HID façade's absolute/relative selection and
   `1/4/2` pointer-state mapping. Extend the feature-level HID API so releases
   retain independent button state, then add typed absolute-sync/focus/reset
   operations where the backend contract needs them; do not emulate them with
   `0x0201`.
8. Rebuild the native AVMP login/discovery/map/read/release/**close** state
   machine from the recorded iDRAC8 native layouts, including heartbeat,
   `ClientStatusMessage`, and `SessionClose`, with byte-level regression tests
   before enabling legacy virtual media by default.
9. Preserve confirmed `0x0301`, but send it only after the display state
   reaches `Running`.

## Static coverage matrix

The following bounds the phrase “complete protocol” for this repository. A
row is complete only when its wire layouts and control flow are covered by the
official client; a product feature may still be deliberately unsupported.

| Area | iDRAC6 source | iDRAC8 source | Current evidence status |
|---|---|---|---|
| APCP setup/TLS/close | KVM package + VM classes | `com.avocent.d.a`, `c.f`, `d.g.a.a` | typed iDRAC6/8 request/profile parsers, transport selection, close path, and byte oracles implemented; iDRAC8 session-id-gated reconnect/remap is implemented, while OTP emission remains gated |
| Console login/status | `kvm.b.a.y`, `tb`, `AVMPStatus` | `c.d.ib`, `d.d.a.c` | all three request encoders and both response layouts are parsed with numeric status preservation; profile-only branches remain gated |
| Video attach/start | `kvm.b.p` | `d.d.b`, `d.g.a.b`, `c.d.ic/fc` | direct attach and dual-random auth implemented; iDRAC8 retains only source-confirmed `0x8102` handles, rebuilds both sockets with the retained-session control writer, re-authenticates and re-attaches video after recoverable transport loss (including the two official five-second retries); capability/scaling and block-video decoding remain unsupported |
| Display framing/types | `kvm.b.a.a` | `d.g.a.a`, `kvm.a.a.j` | all eight types mapped; decoder coverage incomplete |
| Video controls/DVC | legacy message package | `c.d.p/q/eb/kb/t/s` | byte encoders confirmed; high-level setting semantics intentionally unnamed |
| Keyboard/mouse | legacy message package | `c.d.f/rc/sc/...`, `a.b.w`, `app.c.g` | packet encoders and three-button mapping implemented; stateful independent-release mask is covered by an oracle; dedicated focus/sync operations remain unexposed |
| AVMP readonly media | `vm.PacketAVMP`, `ApplianceSession` | unstripped `libVMAPI_DLL.so` | generation-scoped setup/login/discovery/read/map/release/close, configured heartbeat, and iDRAC8 SessionId-gated reconnect/remap implemented; non-readonly modes remain disabled |
| AVMP writable/TOC/options | constants only | unstripped packet serializers, parsers, and request processor | parser and state boundaries recorded; writable flow and native-buggy TOC sender remain unsupported |
| AVMP server image | no supplied iDRAC6 evidence | unstripped serializers, manager, and upload thread | request/response layouts and lifecycle recorded; unsafe native create serializer means product support remains blocked |
| Sharing/preemption/chat/NextBoot | present | present | catalogued as unsupported; no backend feature is permitted to emit them yet |

An iDRAC7 official client artifact has not been supplied. It must be compared
against these encoders before a generation-wide iDRAC6/7/8 claim is made.
Until then, the implementation may share the protocol family only behind
runtime negotiation and explicit faults; it may not infer iDRAC7 behavior from
the iDRAC6 and iDRAC8 binaries.

Every change needs a wire/state regression test and a redacted iDRAC6/iDRAC8
hardware outcome before operational support is claimed.
