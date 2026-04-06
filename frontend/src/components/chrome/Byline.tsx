import React from "react";
import { Children, isValidElement } from "react";
import { Text } from "ink";
import { TEXT_DIM } from "../../theme.js";

interface Props {
  children: React.ReactNode;
}

export function Byline({ children }: Props) {
  const parts = Children.toArray(children);
  if (parts.length === 0) {
    return null;
  }

  return (
    <>
      {parts.map((child, index) => (
        <React.Fragment
          key={isValidElement(child) ? (child.key ?? index) : index}
        >
          {index > 0 ? <Text color={TEXT_DIM}> · </Text> : null}
          {child}
        </React.Fragment>
      ))}
    </>
  );
}
