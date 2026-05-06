export function SignIn() {
  return (
    <div className="mx-auto flex max-w-md flex-col gap-6 pt-12">
      <header className="flex flex-col gap-2">
        <h1 className="text-2xl font-semibold tracking-tight">Sign in to Cellora</h1>
        <p className="text-sm leading-relaxed text-neutral-400">
          We use GitHub to authenticate. The OAuth flow is being wired up — sign-in is not active
          yet.
        </p>
      </header>
      <button
        type="button"
        disabled
        aria-disabled="true"
        className="flex items-center justify-center gap-2 rounded-md border border-neutral-800 bg-neutral-900 px-4 py-2.5 text-sm font-medium text-neutral-200 opacity-60"
      >
        Continue with GitHub
      </button>
      <p className="text-xs text-neutral-500">
        By signing in you agree to the terms of service and privacy policy (links coming with the
        public beta).
      </p>
    </div>
  );
}
