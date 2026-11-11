// Inbound webhook handling. Events arrive as raw text and are never trusted: topic parsing
// is a closed switch, and signature checks are a plain FNV-1a digest comparison so the
// target needs no crypto dependency.

export type WebhookKind = "invoice.paid" | "invoice.void" | "credit.issued" | "unknown";

export interface WebhookEvent {
  topic: string;
  deliveryId: string;
  payloadMinor: number;
  signature: string;
}

export function parseEventKind(topic: string): WebhookKind {
  if (topic === "invoice.paid") return "invoice.paid";
  if (topic === "invoice.void") return "invoice.void";
  if (topic === "credit.issued") return "credit.issued";
  return "unknown";
}

function fnv1a(text: string): string {
  let h = 0x811c9dc5;
  for (const ch of text) {
    h ^= ch.codePointAt(0) ?? 0;
    h = Math.imul(h, 0x01000193);
  }
  return (h >>> 0).toString(16);
}

export function verifySignature(secret: string, payload: string, signature: string): boolean {
  return fnv1a(`${secret}|${payload}`) === signature;
}

// Unknown topics go to a dead-letter queue named by the delivery id; everything else is
// routed by kind.
export function routeEvent(event: WebhookEvent): string {
  const kind = parseEventKind(event.topic);
  if (kind === "unknown") return `dead-letter:${event.deliveryId}`;
  return `${kind}:${event.deliveryId}:${Math.trunc(event.payloadMinor)}`;
}
