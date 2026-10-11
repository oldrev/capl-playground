const measurementId = import.meta.env.VITE_GA_MEASUREMENT_ID?.trim();

declare global {
  interface Window {
    dataLayer?: unknown[];
    gtag?: (...args: unknown[]) => void;
  }
}

export function initAnalytics(): void {
  if (!measurementId || typeof document === "undefined") return;
  if (document.querySelector('script[data-capl-google-analytics="true"]')) return;
  window.dataLayer = window.dataLayer || [];
  window.gtag = (...args: unknown[]) => window.dataLayer?.push(args);
  window.gtag("js", new Date());
  window.gtag("config", measurementId, { send_page_view: true });
  const script = document.createElement("script");
  script.async = true;
  script.dataset.caplGoogleAnalytics = "true";
  script.src = `https://www.googletagmanager.com/gtag/js?id=${encodeURIComponent(measurementId)}`;
  document.head.appendChild(script);
}

export function trackEvent(name: string, parameters?: Record<string, unknown>): void {
  if (measurementId && window.gtag) window.gtag("event", name, parameters);
}
