const HEADER_TERMINATOR = new Uint8Array([13, 10, 13, 10])
const MAX_HEADER_BYTES = 8 * 1024
const MAX_FRAME_BYTES = 64 * 1024 * 1024

function findSequence(haystack: Uint8Array, needle: Uint8Array): number {
  const limit = haystack.length - needle.length
  for (let offset = 0; offset <= limit; offset++) {
    let matches = true
    for (let index = 0; index < needle.length; index++) {
      if (haystack[offset + index] !== needle[index]) {
        matches = false
        break
      }
    }
    if (matches) return offset
  }
  return -1
}

function appendBytes(current: Uint8Array, incoming: Uint8Array): Uint8Array {
  if (current.length === 0) return incoming
  if (incoming.length === 0) return current
  const joined = new Uint8Array(current.length + incoming.length)
  joined.set(current)
  joined.set(incoming, current.length)
  return joined
}

/** Incrementally parses multipart MJPEG using Content-Length.
 *
 * Browser-native multipart image decoding does not expose individual parts
 * and may retain a complete JPEG until the following part arrives. This
 * parser emits as soon as the declared JPEG bytes are present, independent of
 * network chunking and without scanning compressed image data for markers.
 */
export class MjpegPartParser {
  private buffered: Uint8Array<ArrayBufferLike> = new Uint8Array(0)
  private expectedFrameBytes: number | null = null

  push(chunk: Uint8Array): Uint8Array[] {
    this.buffered = appendBytes(this.buffered, chunk)
    const frames: Uint8Array[] = []

    while (true) {
      if (this.expectedFrameBytes === null) {
        const headerEnd = findSequence(this.buffered, HEADER_TERMINATOR)
        if (headerEnd < 0) {
          if (this.buffered.length > MAX_HEADER_BYTES) {
            throw new Error('mjpeg_header_too_large')
          }
          break
        }
        if (headerEnd > MAX_HEADER_BYTES) throw new Error('mjpeg_header_too_large')

        const header = new TextDecoder('ascii').decode(this.buffered.subarray(0, headerEnd))
        const match = /(?:^|\r\n)Content-Length:\s*(\d+)(?:\r\n|$)/i.exec(header)
        if (!match) throw new Error('mjpeg_content_length_missing')
        const frameBytes = Number(match[1])
        if (!Number.isSafeInteger(frameBytes) || frameBytes <= 0) {
          throw new Error('mjpeg_content_length_invalid')
        }
        if (frameBytes > MAX_FRAME_BYTES) throw new Error('mjpeg_frame_too_large')

        this.expectedFrameBytes = frameBytes
        this.buffered = this.buffered.subarray(headerEnd + HEADER_TERMINATOR.length)
      }

      if (this.buffered.length < this.expectedFrameBytes) break

      // subarray keeps parser-side delivery zero-copy. Blob construction at
      // the rendering boundary owns any copy required by the browser.
      frames.push(this.buffered.subarray(0, this.expectedFrameBytes))
      this.buffered = this.buffered.subarray(this.expectedFrameBytes)
      this.expectedFrameBytes = null
    }

    return frames
  }
}

export async function readMjpegFrames(
  stream: ReadableStream<Uint8Array>,
  signal: AbortSignal,
  onFrame: (jpeg: Uint8Array) => void,
): Promise<void> {
  const parser = new MjpegPartParser()
  const reader = stream.getReader()
  try {
    while (true) {
      if (signal.aborted) return
      const { value, done } = await reader.read()
      if (signal.aborted) return
      if (done) return
      if (!value) continue
      for (const frame of parser.push(value)) {
        if (signal.aborted) return
        onFrame(frame)
      }
    }
  } finally {
    reader.releaseLock()
  }
}

/** Owns one browser MJPEG request and all Blob URLs produced from it.
 *
 * The caller receives renderable URLs and reports successful image loads via
 * frameRendered(). Abort, stream replacement, and URL revocation remain
 * transport details rather than leaking into the video lifecycle composable.
 */
export class MjpegBrowserTransport {
  private controller: AbortController | null = null
  private currentObjectUrl = ''
  private readonly retiredObjectUrls = new Set<string>()

  async start(url: string, onFrameUrl: (url: string) => void): Promise<void> {
    this.stop()
    const controller = new AbortController()
    this.controller = controller

    try {
      const response = await fetch(url, { cache: 'no-store', signal: controller.signal })
      if (!response.ok) throw new Error(`mjpeg_http_${response.status}`)
      if (!response.body) throw new Error('mjpeg_response_body_missing')
      const contentType = response.headers.get('content-type')?.toLowerCase() ?? ''
      if (!contentType.startsWith('multipart/x-mixed-replace')) {
        throw new Error('mjpeg_content_type_invalid')
      }

      await readMjpegFrames(response.body, controller.signal, (jpeg) => {
        this.publish(jpeg, onFrameUrl)
      })
      if (!controller.signal.aborted) throw new Error('mjpeg_stream_ended')
    } catch (error) {
      if (!controller.signal.aborted) throw error
    } finally {
      if (this.controller === controller) this.controller = null
    }
  }

  frameRendered(): void {
    for (const url of this.retiredObjectUrls) URL.revokeObjectURL(url)
    this.retiredObjectUrls.clear()
  }

  stop(): void {
    this.controller?.abort()
    this.controller = null
    if (this.currentObjectUrl) URL.revokeObjectURL(this.currentObjectUrl)
    this.currentObjectUrl = ''
    this.frameRendered()
  }

  private publish(jpeg: Uint8Array, onFrameUrl: (url: string) => void): void {
    // BlobPart requires an ArrayBuffer-owned view; this is the single browser
    // rendering-boundary copy after zero-copy multipart parsing.
    const ownedJpeg = new Uint8Array(jpeg)
    const nextUrl = URL.createObjectURL(new Blob([ownedJpeg], { type: 'image/jpeg' }))
    if (this.currentObjectUrl) this.retiredObjectUrls.add(this.currentObjectUrl)
    this.currentObjectUrl = nextUrl
    onFrameUrl(nextUrl)

    // A load event normally releases every retired URL. Bound memory if image
    // decoding falls behind and intermediate frames are superseded.
    while (this.retiredObjectUrls.size > 8) {
      const oldest = this.retiredObjectUrls.values().next().value
      if (typeof oldest !== 'string') break
      URL.revokeObjectURL(oldest)
      this.retiredObjectUrls.delete(oldest)
    }
  }
}
