import type { AuthSession } from "../auth/types";
import { apiFetch } from "./client";

export interface LoginInput {
  login_name: string;
  password: string;
}

export function getSession(): Promise<AuthSession> {
  return apiFetch<AuthSession>("/auth/session", {
    unauthorized: "ignore",
  });
}

export function login(input: LoginInput): Promise<AuthSession> {
  return apiFetch<AuthSession>("/auth/login", {
    method: "POST",
    body: input,
    csrf: "omit",
    unauthorized: "ignore",
  });
}

export function logout(): Promise<void> {
  return apiFetch<void>("/auth/session", {
    method: "DELETE",
    unauthorized: "ignore",
  });
}
