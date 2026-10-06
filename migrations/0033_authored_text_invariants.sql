-- A scoped FK prevents cross-source bodies. Guard representation and make
-- authored UTF-8 bytes immutable once a version has been committed.
CREATE FUNCTION knowledge_authored_text_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP <> 'INSERT' THEN
        RAISE EXCEPTION 'authored source text is immutable; create a new source version';
    END IF;
    PERFORM 1 FROM knowledge_source_versions version
      WHERE version.operator_id=NEW.operator_id
        AND version.tenant_id=NEW.tenant_id
        AND version.project_id=NEW.project_id
        AND version.source_id=NEW.source_id
        AND version.source_version_id=NEW.source_version_id
        AND version.representation='authored_text';
    IF NOT FOUND THEN
        RAISE EXCEPTION 'authored text requires an authored source version';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER knowledge_authored_text_no_rewrite
    BEFORE INSERT OR UPDATE OR DELETE ON knowledge_authored_text
    FOR EACH ROW EXECUTE FUNCTION knowledge_authored_text_guard();

CREATE FUNCTION knowledge_version_representation_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF OLD.representation IS DISTINCT FROM NEW.representation THEN
        RAISE EXCEPTION 'source version representation is immutable';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER knowledge_version_representation_no_rewrite
    BEFORE UPDATE ON knowledge_source_versions
    FOR EACH ROW EXECUTE FUNCTION knowledge_version_representation_guard();
