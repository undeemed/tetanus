// What `src/` is allowed to assume about `upstream/`.
//
// The vendored tree is excluded from the type-check (see `tsconfig.json`), so
// these declarations are the boundary: they say a module exists and nothing
// about its shape. That is honest rather than lazy - a hand-written shape here
// would be a second guess at upstream's contract, checked by nobody, and would
// go stale silently on the next refresh. The shape that IS checked is the one
// this panel produces, in `src/timeline.ts`, and it is checked by the screen
// rendering and by `crates/host/tests/panel_port.rs`.

declare module '@deepseek-ai/dsh-client-ui-conversation/client/chat/ChatView.tsx' {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  export const ChatView: (props: any) => JSX.Element
}

declare module '@deepseek-ai/dsh-client-ui-conversation/client/chat/AssistantNodeView.tsx' {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  export const AssistantNodeView: (props: any) => JSX.Element
}

declare module '@deepseek-ai/dsh-client-ui-conversation/client/chat/MessageItem.tsx' {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  export const UserMessageNodeView: (props: any) => JSX.Element
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  export const UnknownNodeView: (props: any) => JSX.Element
}

declare module '@deepseek-ai/dsh-client-ui-tool/client/tool/ToolCallTree.tsx' {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  export const ToolCallTree: (props: any) => JSX.Element
}

declare module '@deepseek-ai/dsh-client-ui-tool/client/tool/toolviews/GenericToolCard.tsx' {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  export const GenericToolCard: (props: any) => JSX.Element
}

declare module '@deepseek-ai/dsh-client-ui-conversation/client/locales.ts' {
  export const en: Record<string, string>
  export const zh: Record<string, string>
}

declare module '*.module.css' {
  const classes: Record<string, string>
  export default classes
}

declare module '@deepseek-ai/dsh-client-ui-theme/styles/base.css'
declare module '@deepseek-ai/dsh-client-ui-theme/styles/design-platform.css'
declare module '@deepseek-ai/dsh-client-ui-theme/styles/scrollbar.css'
declare module '@deepseek-ai/dsh-client-ui-theme/styles/shiki.css'
