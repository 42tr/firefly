// Only native, successfully saved image results become visible reply cards.
export function generatedImageOutput(payload) {
  const result = payload?.result
  return payload?.name === 'image_generation' && !payload.is_error && !payload.error
    && result?.type === 'image' && !result.error && !result.failure
    && typeof result.image_url === 'string' && result.image_url.startsWith('/api/assistant/attachments/by-file/')
    ? result : null
}
