export interface UseMarkdown {
  renderMarkdown(content: string, options?: { resolveUrl?: (url: string) => string }): string
  safeParseJson(raw: unknown): unknown | null
}

export declare function useMarkdown(): UseMarkdown
