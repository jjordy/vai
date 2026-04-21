import { betterAuth } from "better-auth";
import pg from "pg";

const pool = new pg.Pool({
  connectionString: process.env.DATABASE_URL,
});

/** Better Auth server instance. No vaiApiKey in additionalFields — API keys
 *  are managed entirely by the vai Rust server. */
export const auth = betterAuth({
  database: {
    type: "pg",
    db: pool,
  },
  secret: process.env.BETTER_AUTH_SECRET,
  emailAndPassword: {
    enabled: true,
  },
  user: {
    additionalFields: {},
  },
});
