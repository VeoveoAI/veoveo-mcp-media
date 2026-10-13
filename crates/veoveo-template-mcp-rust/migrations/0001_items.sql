-- The server's tables live in its own schema. Every row belongs to one tenant, every
-- table has a primary key, and its comment states its retention.
CREATE TABLE template.item (
    tenant uuid NOT NULL,
    item_key uuid NOT NULL,
    name text NOT NULL CHECK (octet_length(name) BETWEEN 1 AND 256),
    PRIMARY KEY (tenant, item_key)
);

COMMENT ON TABLE template.item IS
    'Retention: for the life of the tenant. The server deletes no item.';
