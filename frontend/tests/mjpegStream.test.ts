import assert from 'node:assert/strict'
import test from 'node:test'

import {
  MjpegBrowserTransport,
  MjpegPartParser,
  readMjpegFrames,
} from '../src/features/video/mjpegStream.ts'

const text = new TextEncoder()

function join(...chunks: Uint8Array[]): Uint8Array {
  const result = new Uint8Array(chunks.reduce((total, chunk) => total + chunk.length, 0))
  let offset = 0
  for (const chunk of chunks) {
    result.set(chunk, offset)
    offset += chunk.length
  }
  return result
}

function part(jpeg: Uint8Array): Uint8Array {
  return join(
    text.encode(`--frame\r\nContent-Type: image/jpeg\r\nContent-Length: ${jpeg.length}\r\n\r\n`),
    jpeg,
    text.encode('\r\n'),
  )
}

test('emits a complete JPEG without waiting for the next multipart boundary', () => {
  const jpeg = new Uint8Array([0xff, 0xd8, 0x41, 0xff, 0xd9])
  const frames = new MjpegPartParser().push(part(jpeg))

  assert.equal(frames.length, 1)
  assert.deepEqual(frames[0], jpeg)
})

test('parses headers and JPEG data split across arbitrary network chunks', () => {
  const first = new Uint8Array([0xff, 0xd8, 1, 2, 0xff, 0xd9])
  const second = new Uint8Array([0xff, 0xd8, 3, 4, 0xff, 0xd9])
  const wire = join(part(first), part(second))
  const parser = new MjpegPartParser()
  const frames: Uint8Array[] = []

  for (let offset = 0; offset < wire.length; offset += 3) {
    frames.push(...parser.push(wire.subarray(offset, offset + 3)))
  }

  assert.deepEqual(frames, [first, second])
})

test('uses Content-Length even when JPEG data contains boundary-like bytes', () => {
  const jpeg = join(
    new Uint8Array([0xff, 0xd8]),
    text.encode('\r\n--frame\r\n'),
    new Uint8Array([0xff, 0xd9]),
  )
  const frames = new MjpegPartParser().push(part(jpeg))

  assert.deepEqual(frames, [jpeg])
})

test('rejects a multipart part without Content-Length', () => {
  const parser = new MjpegPartParser()
  assert.throws(
    () => parser.push(text.encode('--frame\r\nContent-Type: image/jpeg\r\n\r\n')),
    /mjpeg_content_length_missing/,
  )
})

test('a read completed before cancellation cannot publish after cancellation', async () => {
  let input!: ReadableStreamDefaultController<Uint8Array>
  const stream = new ReadableStream<Uint8Array>({ start(controller) { input = controller } })
  const controller = new AbortController()
  const frames: Uint8Array[] = []
  const reading = readMjpegFrames(stream, controller.signal, frame => frames.push(frame))
  input.enqueue(part(new Uint8Array([1, 2, 3])))
  controller.abort()
  input.close()
  await reading
  assert.deepEqual(frames, [])
})

test('stopping from a frame callback suppresses the remaining frames in that chunk', async () => {
  const originalFetch = globalThis.fetch
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(join(part(new Uint8Array([1])), part(new Uint8Array([2]))))
      controller.close()
    },
  })
  globalThis.fetch = async () => new Response(stream, {
    headers: { 'Content-Type': 'multipart/x-mixed-replace; boundary=frame' },
  })
  const transport = new MjpegBrowserTransport()
  const urls: string[] = []
  try {
    await transport.start('http://mjpeg.test/stream', url => {
      urls.push(url)
      transport.stop()
    })
    assert.equal(urls.length, 1)
  } finally {
    transport.stop()
    globalThis.fetch = originalFetch
  }
})

test('browser transport publishes a Blob before the stream ends', async () => {
  const jpeg = new Uint8Array([0xff, 0xd8, 7, 8, 0xff, 0xd9])
  let closeStream: (() => void) | undefined
  const stream = new ReadableStream<Uint8Array>({
    start(controller) {
      controller.enqueue(part(jpeg))
      closeStream = () => controller.close()
    },
  })
  const originalFetch = globalThis.fetch
  globalThis.fetch = async () => new Response(stream, {
    headers: { 'Content-Type': 'multipart/x-mixed-replace; boundary=frame' },
  })

  const transport = new MjpegBrowserTransport()
  let publishFrame: ((url: string) => void) | undefined
  const published = new Promise<string>((resolve) => {
    publishFrame = resolve
  })
  const running = transport.start('http://mjpeg.test/stream', (url) => publishFrame?.(url))

  try {
    const frameUrl = await published
    assert.deepEqual(new Uint8Array(await (await originalFetch(frameUrl)).arrayBuffer()), jpeg)

    closeStream?.()
    await assert.rejects(running, /mjpeg_stream_ended/)
  } finally {
    transport.stop()
    globalThis.fetch = originalFetch
  }
})
