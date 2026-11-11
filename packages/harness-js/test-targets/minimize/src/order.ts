// Step 8 test target — the input the minimizer is measured against.
//
// `order-input.json` beside this file is a 1842-byte order: migration noise in every field, and
// a first line with no `labels`. Only that missing field matters, so the whole order has to
// shrink down to it. The minimized input still throws the same `TypeError` at the same line,
// which is what the minimizer is required to preserve.

export interface Line {
  sku: string;
  qty: number;
  labels: string[];
}

export interface Order {
  id: string;
  note: string;
  lines: Line[];
}

export function describe(order: Order): string {
  let text = `${order.id}:`;
  for (const line of order.lines) {
    text += ` ${line.sku} x${line.qty}`;
  }
  return `${text} [${order.lines[0].labels.join("|")}]`;
}
