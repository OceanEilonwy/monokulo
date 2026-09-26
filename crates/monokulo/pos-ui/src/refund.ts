// Refund-address QR decoding for the POS payment screen: from an uploaded
// image or the camera, with the same address rules as the customer checkout
// (static/checkout.js). jsQR is the library the checkout already serves; it
// is loaded only when a merchant actually scans.

type JsQr = (data: Uint8ClampedArray, width: number, height: number, options?: object) => { data: string } | null;
declare global { interface Window { jsQR?: JsQr } }

/** A standard (95) or integrated (106) Monero address, by shape. The
 * server checks it properly (network included) when it is saved. */
export function looksLikeAddress(value: string): boolean {
  return /^(?:[1-9A-HJ-NP-Za-km-z]{95}|[1-9A-HJ-NP-Za-km-z]{106})$/.test(value);
}

/** The address inside a QR payload: a bare address or a `monero:` URI. */
export function addressFromQr(payload: string): string | null {
  let data = payload.trim();
  const uri = /^monero:([^?]+)/i.exec(data);
  if (uri) data = decodeURIComponent(uri[1]);
  return looksLikeAddress(data) ? data : null;
}

let loading: Promise<JsQr> | null = null;
function loadJsQr(): Promise<JsQr> {
  if (window.jsQR) return Promise.resolve(window.jsQR);
  loading ??= new Promise((resolve, reject) => {
    const script = document.createElement('script');
    script.src = '/static/jsQR.js';
    script.onload = () => window.jsQR ? resolve(window.jsQR) : reject(new Error('QR decoder unavailable'));
    script.onerror = () => { loading = null; reject(new Error('QR decoder unavailable')); };
    document.head.appendChild(script);
  });
  return loading;
}

function decode(jsQR: JsQr, source: CanvasImageSource, width: number, height: number): string | null {
  const scale = Math.min(1, 1600 / Math.max(width, height));
  const canvas = document.createElement('canvas');
  canvas.width = Math.max(1, Math.round(width * scale));
  canvas.height = Math.max(1, Math.round(height * scale));
  const context = canvas.getContext('2d', { willReadFrequently: true });
  if (!context) return null;
  context.drawImage(source, 0, 0, canvas.width, canvas.height);
  const image = context.getImageData(0, 0, canvas.width, canvas.height);
  return jsQR(image.data, canvas.width, canvas.height, { inversionAttempts: 'attemptBoth' })?.data ?? null;
}

/** The QR payload in an image file, or `null` when it holds no QR code. */
export async function decodeImageFile(file: File): Promise<string | null> {
  const jsQR = await loadJsQr();
  const bitmap = await createImageBitmap(file);
  try { return decode(jsQR, bitmap, bitmap.width, bitmap.height); }
  finally { bitmap.close(); }
}

/** Starts the rear camera in `video` and resolves with the first QR payload
 * seen. `stop()` ends the scan (resolving with `null`). */
export function scanCamera(video: HTMLVideoElement): { result: Promise<string | null>; stop: () => void } {
  let stream: MediaStream | null = null;
  let timer: number | undefined;
  let finish: (value: string | null) => void = () => {};
  const stop = () => {
    window.clearTimeout(timer);
    stream?.getTracks().forEach(track => track.stop());
    stream = null;
    video.srcObject = null;
    finish(null);
  };
  const result = new Promise<string | null>((resolve, reject) => {
    finish = value => { finish = () => {}; resolve(value); };
    (async () => {
      const jsQR = await loadJsQr();
      stream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: 'environment' }, audio: false });
      video.srcObject = stream;
      await video.play();
      const frame = () => {
        if (!stream) return;
        if (video.readyState >= 2 && video.videoWidth > 0) {
          const payload = decode(jsQR, video, Math.min(video.videoWidth, 640), Math.round(video.videoHeight * Math.min(video.videoWidth, 640) / video.videoWidth));
          if (payload) { const done = finish; finish = () => {}; stop(); done(payload); return; }
        }
        timer = window.setTimeout(frame, 200);
      };
      frame();
    })().catch(error => { stop(); reject(error); });
  });
  return { result, stop };
}
