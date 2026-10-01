export function createHostYielder(host) {
  if (typeof host.MessageChannel === "function") {
    let channel;
    const pending = [];
    return () => new Promise(resolve => {
      if (!channel) {
        channel = new host.MessageChannel();
        channel.port1.onmessage = () => pending.shift()();
      }
      pending.push(resolve);
      channel.port2.postMessage(0);
    });
  }
  return () => new Promise(resolve => host.setTimeout(resolve, 0));
}

export const yieldToHost = createHostYielder(globalThis);
