self.onmessage = async ({ data: { packagePath, options } }) => {
  try {
    const bts = await import(`/${packagePath}/bts.js`);
    await bts.default();
    const source = 'trace "worker" { output = uuid() }';
    self.postMessage({ version: bts.version(), reference: bts.languageReference(), diagnostics: bts.check(source).length,
      events: bts.generate(source, options).events.length });
  } catch (error) {
    self.postMessage({ error: String(error) });
  }
};
