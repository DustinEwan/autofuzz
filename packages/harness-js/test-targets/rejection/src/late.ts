// The target the rejection test invokes. The dropped promise is the shape of a real unawaited async
// path: the call returns normally and answers, and the failure lands afterwards, when the worker has
// no open request left to report it against.
export function fireLater(delayMs: number): string {
  void new Promise((_resolve, reject) => {
    setTimeout(() => reject(new Error("late")), delayMs);
  });
  return "fired";
}
