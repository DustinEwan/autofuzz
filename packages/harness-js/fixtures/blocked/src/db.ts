/**
 * A handle the generator cannot build: a class instance, so only a fixture can supply one, and a
 * private field so not even a hand-written object literal stands in for it. Every entry point
 * that takes one is skipped with `unconstructible-parameter`, and this file is the `module` the
 * blocker names.
 */
export class Db {
  #connection: unknown;

  constructor(connection: unknown) {
    this.#connection = connection;
  }

  query(sql: string): unknown[] {
    void this.#connection;
    return [sql];
  }
}
