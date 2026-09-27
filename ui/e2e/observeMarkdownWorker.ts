/** Test-only worker transport counters; no document contents are recorded. */
export function observeMarkdownWorker() {
  const scope = window as any;
  const stats = {
    created: 0,
    terminated: 0,
    maxConcurrent: 0,
    requests: 0,
    trees: 0,
    errors: 0,
  };
  scope.__SHADOW_MARKDOWN_WORKERS__ = stats;
  window.Worker = new Proxy(window.Worker, {
    construct(target, args) {
      const worker = Reflect.construct(target, args) as Worker;
      stats.created++;
      stats.maxConcurrent = Math.max(
        stats.maxConcurrent,
        stats.created - stats.terminated,
      );
      const post = worker.postMessage.bind(worker);
      worker.postMessage = (message: unknown) => {
        stats.requests++;
        post(message);
      };
      const terminate = worker.terminate.bind(worker);
      worker.terminate = () => {
        stats.terminated++;
        terminate();
      };
      worker.addEventListener("message", (event) => {
        if (event.data?.tree) stats.trees++;
        else stats.errors++;
      });
      worker.addEventListener("error", () => stats.errors++);
      worker.addEventListener("messageerror", () => stats.errors++);
      return worker;
    },
  });
}
