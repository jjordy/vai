import {
  createRootRoute,
  Outlet,
  ScrollRestoration,
} from "@tanstack/react-router";
import { Meta, Scripts } from "@tanstack/start";

export const Route = createRootRoute({
  component: RootComponent,
});

function RootComponent() {
  return (
    <html lang="en">
      <head>
        <meta charSet="UTF-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1" />
        <title>vai</title>
        <style>{`
          *, *::before, *::after { box-sizing: border-box; }
          body { margin: 0; font-family: system-ui, sans-serif; color: #111; background: #fff; }
          a { color: #2563eb; }
          input { display: block; width: 100%; padding: 8px 12px; margin: 4px 0 12px; border: 1px solid #d1d5db; border-radius: 6px; font-size: 14px; }
          button[type=submit], button.primary { background: #111; color: #fff; border: none; border-radius: 6px; padding: 10px 20px; font-size: 14px; cursor: pointer; }
          button[type=submit]:disabled, button.primary:disabled { opacity: 0.5; cursor: not-allowed; }
          label { display: block; font-size: 13px; font-weight: 500; color: #374151; }
        `}</style>
        <Meta />
      </head>
      <body>
        <Outlet />
        <ScrollRestoration />
        <Scripts />
      </body>
    </html>
  );
}
